"""Latency and CPU of alondrad under many busy nodes.

Seeds a throwaway HOME with a production-shaped domain (115 tasks, 669 interactions with long
bodies, one agent per simulated node), starts that alondrad on its own port with no Claude and no
GitHub on its PATH, and drives it with N nodes sending hooks and clients reading views. While the
load runs it times the unsigned /daemon liveness route, `alondra status` (the home and tasks views) and every hook,
and reads the daemon's CPU time.

    make load-bench LOAD_BENCH_ARGS="--nodes 9 --seconds 60"
"""

import argparse
import json
import os
import shutil
import socket
import math
import struct
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request

EVENTS = ["UserPromptSubmit", "PreToolUse", "PostToolUse", "PreToolUse", "PostToolUse", "Stop"]


def cpu_seconds(pid):
    text = subprocess.run(["ps", "-o", "time=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
    return sum(float(x) * 60 ** i for i, x in enumerate(reversed(text.split(":")))) if text else 0.0


def timed(samples, lock, action):
    record = {"started": time.monotonic(), "finished": None, "ok": None, "error": None}
    with lock:
        samples.append(record)
    ok, error = False, None
    try:
        ok = bool(action())
    except Exception as caught:
        ok, error = False, type(caught).__name__
    finally:
        finished = time.monotonic()
        with lock:
            record.update(finished=finished, ok=ok, error=error)


def run_worker(failures, lock, name, target, *args):
    try:
        target(*args)
    except Exception as error:
        with lock:
            failures.append(f"{name}: {type(error).__name__}")


class NoRedirects(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def health(port):
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirects())
    try:
        with opener.open(f"http://127.0.0.1:{port}/daemon", timeout=30) as response:
            if response.status != 200:
                return False
            body = response.read(64 * 1024 + 1)
            if len(body) > 64 * 1024:
                return False
            payload = json.loads(body)
            return isinstance(payload, dict) and isinstance(payload.get("version"), str) and bool(payload["version"].strip())
    except urllib.error.HTTPError as error:
        error.close()
        return False
    except (OSError, ValueError):
        return False


def owns_listener(pid, port):
    try:
        result = subprocess.run(["/usr/sbin/lsof", "-nP", "-a", "-p", str(pid),
            f"-iTCP:{port}", "-sTCP:LISTEN", "-Fp"], capture_output=True, text=True, timeout=3)
    except (OSError, subprocess.TimeoutExpired):
        return False
    return result.returncode == 0 and f"p{pid}" in result.stdout.splitlines()


def stop_process(process):
    if process is None or process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=30)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


def frame(home, body):
    wire = json.dumps(body).encode()
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
            connection.settimeout(30)
            connection.connect(os.path.join(home, "hooks.sock"))
            connection.sendall(struct.pack(">I", len(wire)) + wire)
            reply = b""
            while part := connection.recv(65536):
                reply += part
                if len(reply) > 64 * 1024:
                    return False
                if len(reply) < 2:
                    continue
                status_code = struct.unpack(">H", reply[:2])[0]
                if not 200 <= status_code < 300:
                    return False
                try:
                    response = json.loads(reply[2:])
                except ValueError:
                    continue
                if not isinstance(response, dict):
                    return False
                try:
                    connection.sendall(b"\x06")
                except OSError:
                    pass
                return True
            return False
    except OSError:
        return False


FAILURES = []


def hook(home, session, event, call):
    return frame(home, {"launched": True, "payload": {
        "session_id": session, "hook_event_name": event, "tool_name": "Bash", "tool_use_id": call,
        "tool_input": {"command": "bazelisk test //crates/..."}, "last_assistant_message": "Done."}})


def report(home, session, event, call):
    return frame(home, {"node_event": {"session_id": session, "event": event, "tool": "Bash",
                                       "tool_use_id": call, "input": {"command": "ls"}}})


def mod_connection(home, session, stop):
    """The connection a node's mod holds open; the daemon writes its outbox down it."""
    wire = json.dumps({"node_connect": {"session_id": session, "version": "bench"}}).encode()
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
            connection.settimeout(1)
            connection.connect(os.path.join(home, "hooks.sock"))
            connection.sendall(struct.pack(">I", len(wire)) + wire)
            while not stop.is_set():
                try:
                    if not connection.recv(65536):
                        return
                except socket.timeout:
                    continue
    except OSError:
        return


def status(cli, env):
    try:
        run = subprocess.run([cli, "status"], env=env, capture_output=True, text=True, timeout=30)
    except subprocess.TimeoutExpired:
        FAILURES.append("alondra status: no answer in 30 s")
        return False
    if run.returncode != 0:
        FAILURES.append(f"alondra status: {run.stderr.strip()[-200:]}")
    return run.returncode == 0


def latency_summary(times):
    times = sorted(times)
    def pick(quantile):
        if not times:
            return None
        return round(times[max(0, math.ceil(quantile * len(times)) - 1)] * 1000)
    return {"n": len(times), "p50_ms": pick(0.5), "p95_ms": pick(0.95), "p99_ms": pick(0.99),
            "max_ms": round(times[-1] * 1000) if times else None, "over_5s": sum(t > 5 for t in times)}


def summary(samples):
    accepted = latency_summary(t for t, ok in samples if ok)
    errors = latency_summary(t for t, ok in samples if not ok)
    accepted_n = accepted.pop("n")
    return {"n": len(samples), "failed": errors["n"], "accepted_n": accepted_n,
            **accepted, "error_latency": errors}


def window_summary(records, start, end):
    offered = [record for record in records if start <= record["started"] < end]
    steady = [record for record in offered if record["finished"] is not None and record["finished"] <= end]
    drain = [record for record in offered if record["finished"] is not None and record["finished"] > end]
    def latencies(rows):
        return summary([(row["finished"] - row["started"], row["ok"]) for row in rows])
    return {**latencies(steady), "offered": len(offered),
            "pending_at_boundary": len(offered) - len(steady),
            "drain": latencies(drain), "unfinished": len(offered) - len(steady) - len(drain),
            "excluded": len(records) - len(offered)}


def verdict(result):
    reasons = []
    cohorts = ["health", "view", "hook"] + (["report"] if "report" in result else [])
    for cohort in cohorts:
        measured = result[cohort]
        if not measured["accepted_n"]:
            reasons.append(f"{cohort}: no successful steady samples")
        if measured["failed"]:
            reasons.append(f"{cohort}: {measured['failed']} failed requests")
        if measured.get("drain", {}).get("failed", 0):
            reasons.append(f"{cohort}: failed requests during drain")
        if measured.get("unfinished", 0):
            reasons.append(f"{cohort}: unfinished requests")
    if result.get("unfinished_workers", 0):
        reasons.append("workers did not finish")
    if result.get("worker_failures"):
        reasons.append("workers failed")
    if result.get("daemon_exited_during_load", False):
        reasons.append("daemon exited during load")
    return {"passed": not reasons, "reasons": reasons}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--daemon", required=True)
    ap.add_argument("--cli", required=True)
    ap.add_argument("--seed", required=True, help="the daemon_engine_test binary")
    ap.add_argument("--nodes", type=int, default=9)
    ap.add_argument("--hook-every", type=float, default=0.25, help="seconds between one node's hooks")
    ap.add_argument("--clients", type=int, default=3)
    ap.add_argument("--burn", type=int, default=0, help="busy processes that load the machine meanwhile")
    ap.add_argument("--seconds", type=int, default=60)
    ap.add_argument("--port", type=int, default=47870)
    ap.add_argument("--label", default="")
    ap.add_argument("--sample", help="write a 10 s `sample` profile of alondrad here")
    args = ap.parse_args()
    if min(args.nodes, args.clients, args.seconds) < 1 or args.burn < 0 or args.hook_every <= 0:
        ap.error("nodes, clients, seconds and hook-every must be positive; burn must be nonnegative")
    FAILURES.clear()

    home = tempfile.mkdtemp(prefix="alondra-load-bench-", dir="/tmp")
    daemon = None
    log = None
    stop = threading.Event()
    workers = []
    started_workers = []
    burners = []
    try:
        seed_env = dict(os.environ, HOME=home, ALONDRA_LOAD_BENCH_DB=os.path.join(home, "domain.db"),
                        ALONDRA_LOAD_BENCH_NODES=str(args.nodes))
        subprocess.run([args.seed, "--ignored", "--exact", "domain_store::load_bench::seed_load_bench"],
                       env=seed_env, check=True, capture_output=True)
        env = dict(os.environ, HOME=home, ALONDRA_HOME=home, ALONDRA_PORT=str(args.port), ALONDRA_SANDBOX="1",
                   ALONDRA_WAKE="0", CLAUDE_HOME=os.path.join(home, "no-claude"), PATH="/usr/bin:/bin")
        log = open(os.path.join(home, "alondrad.out"), "w")
        daemon = subprocess.Popen([args.daemon], env=env, stdout=log, stderr=log)
        deadline = time.time() + 60
        while not (os.path.exists(os.path.join(home, "hooks.sock"))
                   and owns_listener(daemon.pid, args.port) and health(args.port)):
            if daemon.poll() is not None or time.time() > deadline:
                raise SystemExit(f"alondrad did not start: {open(log.name).read()[-2000:]}")
            time.sleep(0.2)
        if not status(args.cli, env):
            raise SystemExit("alondra status failed against the bench daemon")

        lock = threading.Lock()
        samples = {"health": [], "view": [], "hook": [], "report": []}
        worker_failures = []

        def node(n):
            session = f"bench-node-{n}"
            i = 0
            while not stop.is_set():
                call = f"{session}-{i // len(EVENTS)}"
                event = EVENTS[i % len(EVENTS)]
                if event == "PreToolUse":
                    timed(samples["report"], lock, lambda: report(home, session, "tool.start", call))
                timed(samples["hook"], lock, lambda: hook(home, session, event, call))
                if event == "PostToolUse":
                    timed(samples["report"], lock, lambda: report(home, session, "tool.end", call))
                i += 1
                time.sleep(args.hook_every)

        def client():
            while not stop.is_set():
                timed(samples["view"], lock, lambda: status(args.cli, env))
                time.sleep(0.5)

        def prober():
            while not stop.is_set():
                timed(samples["health"], lock, lambda: health(args.port))
                time.sleep(0.5)

        def mod(n):
            mod_connection(home, f"bench-node-{n}", stop)
            if not stop.is_set():
                with lock:
                    worker_failures.append(f"mod-{n}: connection ended during load")

        def worker(name, target, *args):
            return threading.Thread(target=run_worker,
                args=(worker_failures, lock, name, target, *args), daemon=True)

        workers = [worker(f"mod-{n}", mod, n) for n in range(args.nodes)]
        workers += [worker(f"node-{n}", node, n) for n in range(args.nodes)]
        workers += [worker(f"client-{n}", client) for n in range(args.clients)]
        workers.append(worker("prober", prober))
        for w in workers:
            w.start()
            started_workers.append(w)
        for _ in range(args.burn):
            burners.append(subprocess.Popen(["/usr/bin/yes"], stdout=subprocess.DEVNULL))
        time.sleep(5)
        cpu_before, wall_before = cpu_seconds(daemon.pid), time.monotonic()
        if args.sample:
            subprocess.run(["sample", str(daemon.pid), "10", "-file", args.sample], capture_output=True)
            time.sleep(max(0, args.seconds - 10))
        else:
            time.sleep(args.seconds)
        wall_after = time.monotonic()
        cpu = cpu_seconds(daemon.pid) - cpu_before
        wall = wall_after - wall_before
        daemon_exited = daemon.poll() is not None
        stop.set()
        for b in burners:
            b.kill()
            b.wait()
        for w in started_workers:
            w.join(timeout=35)
        stop_process(daemon)
        drain_seconds = time.monotonic() - wall_after
        result = {"schema": 1, "probe_endpoint": "/daemon", "label": args.label, "nodes": args.nodes, "burn": args.burn, "seconds": round(wall, 3),
                  "phases": {"warmup_seconds": 5, "steady_seconds": round(wall, 3), "drain_seconds": round(drain_seconds, 3)},
                  "worker_failures": list(worker_failures), "daemon_exited_during_load": daemon_exited,
                  "cpu_seconds": round(cpu, 3),
                  "unfinished_workers": sum(w.is_alive() for w in started_workers),
                  "cpu_cores": round(cpu / wall, 2), "load_avg": round(os.getloadavg()[0], 1)}
        with lock:
            recorded = {name: [dict(row) for row in rows] for name, rows in samples.items()}
        result.update({name: window_summary(rows, wall_before, wall_after) for name, rows in recorded.items()})
        result["samples"] = {name: [{"started_s": row["started"] - wall_before,
            "finished_s": None if row["finished"] is None else row["finished"] - wall_before,
            "ok": row["ok"], "error": row["error"]} for row in rows] for name, rows in recorded.items()}
        result["view_failures"] = sorted(set(FAILURES))[:5]
        result["verdict"] = verdict(result)
        print(json.dumps(result))
        if not result["verdict"]["passed"]:
            raise SystemExit(1)
    finally:
        stop.set()
        try:
            try:
                for burner in burners:
                    stop_process(burner)
            finally:
                stop_process(daemon)
            for worker in started_workers:
                worker.join(timeout=35)
        finally:
            if log is not None:
                log.close()
            shutil.rmtree(home, ignore_errors=True)


if __name__ == "__main__":
    main()
