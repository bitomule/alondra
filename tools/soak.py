"""Carga sintética para medir la memoria de alondrad durante horas.

Arranca una instancia aparte (otro puerto y otro directorio de datos), le manda hooks de
nodos que nacen y mueren, preguntas que se contestan, informes que se leen y clientes que
esperan cambios, y apunta la memoria residente del proceso cada 5 minutos en un CSV.

    python3 tools/soak.py --hours 72 --out soak.csv
"""

import argparse
import json
import os
import random
import subprocess
import threading
import time
import urllib.request

PORT = 47890


def call(method, path, body=None, timeout=70):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(f"http://127.0.0.1:{PORT}{path}", data=data, method=method,
                                 headers={"content-type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return json.loads(r.read() or b"null")
    except Exception:
        return None


def hooks(stop):
    sessions = [f"soak-{i}" for i in range(20)]
    while not stop.is_set():
        s = random.choice(sessions)
        event = random.choice(["UserPromptSubmit", "PreToolUse", "PostToolUse", "Stop"])
        call("POST", "/hook", {"session_id": s, "hook_event_name": event, "tool_name": "Bash",
                               "tool_use_id": f"t{random.randint(0, 10**9)}",
                               "tool_input": {"command": "echo " + "x" * random.randint(1, 400)},
                               "last_assistant_message": "fin de turno " + "y" * random.randint(1, 800)})
        if random.random() < 0.01:
            i = random.randint(0, 19)
            sessions[i] = f"soak-{i}-{random.randint(0, 10**6)}"
        time.sleep(0.05)


def questions(stop):
    while not stop.is_set():
        s = f"soak-{random.randint(0, 19)}"
        q = call("POST", "/v1/questions", {"session_id": s, "title": "¿Seguimos?", "detail": "z" * 2000,
                                           "options": ["Sí", "No"]})
        r = call("POST", "/v1/reports", {"session_id": s, "title": "Informe", "body": "w" * 3000, "kind": "notice"})
        time.sleep(1)
        if q and "id" in q:
            call("POST", f"/v1/questions/{q['id']}/answer", {"text": "Sí", "option": "Sí"})
        if r and "id" in r:
            call("POST", f"/v1/reports/{r['id']}/read", {})
        time.sleep(1)


def waiter(stop):
    tick = 0
    while not stop.is_set():
        o = call("GET", "/v1/overview")
        if o:
            tick = o.get("tick", tick)
        call("GET", f"/v1/changes?after={tick}&wait=50")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--hours", type=float, default=72)
    ap.add_argument("--out", default="soak.csv")
    ap.add_argument("--binary", default=os.path.expanduser("~/.local/bin/alondrad"))
    args = ap.parse_args()
    home = f"/tmp/alondra-soak-{os.getpid()}"
    os.makedirs(home, exist_ok=True)
    env = dict(os.environ, ALONDRA_HOME=home, ALONDRA_PORT=str(PORT), ALONDRA_HOLD="0",
               CLAUDE_HOME=home + "/no-claude")
    proc = subprocess.Popen([args.binary], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(2)
    stop = threading.Event()
    workers = [threading.Thread(target=f, args=(stop,), daemon=True) for f in [hooks, questions] + [waiter] * 8]
    for w in workers:
        w.start()
    end = time.time() + args.hours * 3600
    with open(args.out, "a") as out:
        out.write("epoch,minutes,rss_kb,events_db_bytes\n")
        start = time.time()
        while time.time() < end and proc.poll() is None:
            rss = subprocess.run(["ps", "-o", "rss=", "-p", str(proc.pid)], capture_output=True, text=True).stdout.strip()
            db = sum(os.path.getsize(os.path.join(home, f)) for f in os.listdir(home) if f.startswith("alondra.db"))
            out.write(f"{int(time.time())},{int((time.time() - start) / 60)},{rss},{db}\n")
            out.flush()
            time.sleep(300)
    stop.set()
    proc.terminate()


if __name__ == "__main__":
    main()
