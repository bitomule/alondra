"""Latency and CPU of alondrad under a busy fleet.

Seeds a throwaway HOME with a production-shaped domain (115 tasks, 669 interactions with long
bodies, one agent per simulated node), starts that alondrad on its own port with no Claude and no
GitHub on its PATH, and drives it with N nodes sending hooks and clients reading views. While the
load runs it times /v1/health, `alondra status` (the home, tasks and fleet views) and every hook,
and reads the daemon's CPU time.

    make load-bench LOAD_BENCH_ARGS="--nodes 9 --seconds 60"
"""

import argparse
import json
import os
import shutil
import socket
import statistics
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
    start = time.monotonic()
    ok = action()
    with lock:
        samples.append((time.monotonic() - start, ok))


def health(port):
    try:
        urllib.request.urlopen(f"http://127.0.0.1:{port}/v1/health", timeout=30).read()
    except urllib.error.HTTPError:
        pass
    except OSError:
        return False
    return True


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
                try:
                    json.loads(reply[2:])
                    break
                except ValueError:
                    continue
            if len(reply) < 2 or struct.unpack(">H", reply[:2])[0] >= 400:
                return False
            try:
                connection.sendall(b"\x06")
            except OSError:
                pass
            return True
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
            connection.connect(os.path.join(home, "hooks.sock"))
            connection.sendall(struct.pack(">I", len(wire)) + wire)
            connection.settimeout(1)
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


def summary(samples):
    times = sorted(t for t, _ in samples)
    if not times:
        return {"n": 0}
    pick = lambda q: round(times[min(len(times) - 1, int(q * len(times)))] * 1000)
    return {"n": len(times), "failed": sum(1 for _, ok in samples if not ok), "p50_ms": pick(0.5),
            "p95_ms": pick(0.95), "max_ms": round(times[-1] * 1000),
            "over_5s": sum(1 for t in times if t > 5)}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--daemon", required=True)
    ap.add_argument("--cli", required=True)
    ap.add_argument("--seed", required=True, help="the alondrad_test binary")
    ap.add_argument("--nodes", type=int, default=9)
    ap.add_argument("--hook-every", type=float, default=0.25, help="seconds between one node's hooks")
    ap.add_argument("--clients", type=int, default=3)
    ap.add_argument("--burn", type=int, default=0, help="busy processes that load the machine meanwhile")
    ap.add_argument("--seconds", type=int, default=60)
    ap.add_argument("--port", type=int, default=47870)
    ap.add_argument("--label", default="")
    ap.add_argument("--sample", help="write a 10 s `sample` profile of alondrad here")
    args = ap.parse_args()

    home = tempfile.mkdtemp(prefix="alondra-load-bench-", dir="/tmp")
    try:
        seed_env = dict(os.environ, ALONDRA_LOAD_BENCH_DB=os.path.join(home, "domain.db"),
                        ALONDRA_LOAD_BENCH_NODES=str(args.nodes))
        subprocess.run([args.seed, "--ignored", "--exact", "domain_store::load_bench::seed_load_bench"],
                       env=seed_env, check=True, capture_output=True)
        env = dict(os.environ, ALONDRA_HOME=home, ALONDRA_PORT=str(args.port), ALONDRA_SANDBOX="1",
                   ALONDRA_WAKE="0", CLAUDE_HOME=os.path.join(home, "no-claude"), PATH="/usr/bin:/bin")
        log = open(os.path.join(home, "alondrad.out"), "w")
        daemon = subprocess.Popen([args.daemon], env=env, stdout=log, stderr=log)
        deadline = time.time() + 60
        while not os.path.exists(os.path.join(home, "hooks.sock")) or not health(args.port):
            if daemon.poll() is not None or time.time() > deadline:
                raise SystemExit(f"alondrad did not start: {open(log.name).read()[-2000:]}")
            time.sleep(0.2)
        if not status(args.cli, env):
            raise SystemExit("alondra status failed against the bench daemon")

        stop = threading.Event()
        lock = threading.Lock()
        samples = {"health": [], "view": [], "hook": []}

        def node(n):
            session = f"bench-node-{n}"
            threading.Thread(target=mod_connection, args=(home, session, stop), daemon=True).start()
            i = 0
            while not stop.is_set():
                call = f"{session}-{i // len(EVENTS)}"
                event = EVENTS[i % len(EVENTS)]
                if event == "PreToolUse":
                    report(home, session, "tool.start", call)
                timed(samples["hook"], lock, lambda: hook(home, session, event, call))
                if event == "PostToolUse":
                    report(home, session, "tool.end", call)
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

        workers = [threading.Thread(target=node, args=(n,), daemon=True) for n in range(args.nodes)]
        workers += [threading.Thread(target=client, daemon=True) for _ in range(args.clients)]
        workers.append(threading.Thread(target=prober, daemon=True))
        for w in workers:
            w.start()
        burners = [subprocess.Popen(["/usr/bin/yes"], stdout=subprocess.DEVNULL) for _ in range(args.burn)]
        time.sleep(5)
        with lock:
            for s in samples.values():
                s.clear()
        cpu_before, wall_before = cpu_seconds(daemon.pid), time.monotonic()
        if args.sample:
            subprocess.run(["sample", str(daemon.pid), "10", "-file", args.sample], capture_output=True)
            time.sleep(max(0, args.seconds - 10))
        else:
            time.sleep(args.seconds)
        cpu = cpu_seconds(daemon.pid) - cpu_before
        wall = time.monotonic() - wall_before
        stop.set()
        for b in burners:
            b.kill()
            b.wait()
        for w in workers:
            w.join(timeout=35)
        daemon.terminate()
        daemon.wait(timeout=30)
        result = {"label": args.label, "nodes": args.nodes, "burn": args.burn, "seconds": round(wall),
                  "cpu_cores": round(cpu / wall, 2), "load_avg": round(os.getloadavg()[0], 1)}
        result.update({k: summary(v) for k, v in samples.items()})
        result["view_failures"] = sorted(set(FAILURES))[:5]
        print(json.dumps(result))
    finally:
        shutil.rmtree(home, ignore_errors=True)


if __name__ == "__main__":
    main()
