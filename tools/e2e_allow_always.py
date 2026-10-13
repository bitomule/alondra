#!/usr/bin/env python3
"""«Allow always» end to end with the real Claude Code, against its own throwaway sandbox.

Claude Code runs for real (`claude -p --permission-mode …`) with HOME in a temporary folder and
ANTHROPIC_BASE_URL pointing at a local server that plays the model: it answers with one Bash call
of the exact command each case needs, then with "DONE". No account, no tokens, deterministic.
Claude Code's PermissionRequest hook is the plugin's own `alondra hook`, talking to a sandbox
daemon (tools/e2e-sandbox.sh); the cards are answered from the sandbox phone, as the apps do.
The repo is a temporary one with two worktrees under `.claude/worktrees/`, as Alondra makes them.
Nothing touches the real ~/.claude, ~/.claude.json or ~/.alondra; everything is removed at the end.

  tools/e2e_allow_always.py <alondrad> <alondra> <alondra-relay> <alondra-sandbox>
"""

import http.server
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import uuid

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from e2e_cards import TIMEOUT, Phone, Sandbox  # noqa: E402

RULE = "Bash(make build *)"


class Model(http.server.ThreadingHTTPServer):
    """Plays the model: a Bash call of `command`, then "DONE" once Claude Code returns its result."""

    command = ""


class ModelHandler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_HEAD(self):
        self.send_response(200)
        self.end_headers()

    def do_GET(self):
        self._json({})

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("content-length", 0))) or b"{}")
        if "count_tokens" in self.path:
            return self._json({"input_tokens": 10})
        messages = body.get("messages", [])
        last = messages[-1]["content"] if messages else ""
        answered = isinstance(last, list) and any(block.get("type") == "tool_result" for block in last)
        has_bash = any(tool.get("name") == "Bash" for tool in body.get("tools", []))
        if has_bash and not answered:
            block = {"type": "tool_use", "id": f"toolu_{uuid.uuid4().hex[:20]}", "name": "Bash",
                     "input": {"command": self.server.command, "description": "Run the case's command"}}
            stop = "tool_use"
        else:
            block = {"type": "text", "text": "DONE"}
            stop = "end_turn"
        message = {"id": f"msg_{uuid.uuid4().hex[:20]}", "type": "message", "role": "assistant", "model": body.get("model", "x"),
                   "content": [block], "stop_reason": stop, "stop_sequence": None,
                   "usage": {"input_tokens": 10, "output_tokens": 10}}
        if not body.get("stream"):
            return self._json(message)
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.end_headers()

        def event(name, data):
            self.wfile.write(f"event: {name}\ndata: {json.dumps(data)}\n\n".encode())

        event("message_start", {"type": "message_start", "message": dict(message, content=[], stop_reason=None)})
        if block["type"] == "tool_use":
            event("content_block_start", {"type": "content_block_start", "index": 0, "content_block": dict(block, input={})})
            event("content_block_delta", {"type": "content_block_delta", "index": 0,
                                           "delta": {"type": "input_json_delta", "partial_json": json.dumps(block["input"])}})
        else:
            event("content_block_start", {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}})
            event("content_block_delta", {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": block["text"]}})
        event("content_block_stop", {"type": "content_block_stop", "index": 0})
        event("message_delta", {"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": None},
                                "usage": {"output_tokens": 10}})
        event("message_stop", {"type": "message_stop"})
        self.wfile.flush()

    def _json(self, value):
        data = json.dumps(value).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def git(cwd, *args):
    subprocess.run(["git", "-C", cwd, *args], check=True, capture_output=True)


class World:
    """The sandbox, the model, a repo with two worktrees and a HOME for Claude Code, all temporary."""

    def __init__(self, binaries):
        self.sandbox = Sandbox(binaries, int(os.environ.get("SANDBOX_PORT", "48050")))
        self.scratch = self.sandbox.scratch
        self.model = Model(("127.0.0.1", 0), ModelHandler)
        threading.Thread(target=self.model.serve_forever, daemon=True).start()
        self.repo = os.path.realpath(os.path.join(self.scratch, "app"))
        os.makedirs(self.repo)
        with open(os.path.join(self.repo, "Makefile"), "w") as makefile:
            makefile.write("build:\n\t@echo built\ntest:\n\t@echo tested\n")
        git(self.repo, "init", "-q", "-b", "main")
        git(self.repo, "add", "Makefile")
        git(self.repo, "-c", "user.name=e2e", "-c", "user.email=e2e@example.com", "commit", "-q", "-m", "first")
        self.worktrees = []
        for name in ("w1", "w2"):
            path = os.path.join(self.repo, ".claude", "worktrees", name)
            git(self.repo, "worktree", "add", "-q", "-b", name, path)
            self.worktrees.append(path)
        self.home = os.path.join(self.scratch, "home")
        os.makedirs(os.path.join(self.home, ".claude"))
        self.asked_log = os.path.join(self.scratch, "asked.jsonl")
        hook = os.path.join(self.scratch, "hook.sh")
        with open(hook, "w") as script:
            script.write(f'#!/bin/sh\npayload=$(cat)\nprintf "%s\\n" "$payload" >> "{self.asked_log}"\n'
                         f'printf "%s" "$payload" | exec "{self.sandbox.alondra}" hook\n')
        os.chmod(hook, 0o755)
        with open(os.path.join(self.home, ".claude", "settings.json"), "w") as settings:
            json.dump({"hooks": {"PermissionRequest": [{"matcher": "*", "hooks": [{"type": "command", "command": hook, "timeout": 600}]}]}},
                      settings)
        self.phone = Phone(self.sandbox, direct=True)
        self.connections = []

    @property
    def settings_file(self):
        return os.path.join(self.repo, ".claude", "settings.local.json")

    def rules(self):
        try:
            return json.load(open(self.settings_file)).get("permissions", {}).get("allow", [])
        except FileNotFoundError:
            return []

    def node(self, session, cwd):
        """The node's connection to the daemon, as the plugin's mod opens it, so its prompts are held."""
        env = self.sandbox.env
        key = f"e2e-allow-{session[:8]}"
        sessions = os.path.join(env["ALONDRA_HOME"], "runtime", "sessions")
        os.makedirs(sessions, exist_ok=True)
        settings = os.path.join(sessions, f"{key}.json")
        with open(settings, "w") as file:
            json.dump({"env": {"ALONDRA_DOMAIN_AGENT": session}}, file)
        os.chmod(settings, 0o600)
        connection = subprocess.Popen([self.sandbox.alondra, "node", "connect"], env=env, stdin=subprocess.PIPE,
                                      stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        connection.stdin.write(json.dumps({"session_id": session, "version": "2.1.291", "cwd": cwd, "launch_key": key}))
        connection.stdin.close()
        assert connection.stdout.readline() == "connected\n", "the node could not connect"
        threading.Thread(target=lambda: [None for _ in connection.stdout], daemon=True).start()
        self.connections.append(connection)
        deadline = time.time() + TIMEOUT
        while "ALONDRA_DOMAIN_CAPABILITY" not in json.load(open(settings))["env"]:
            assert time.time() < deadline, "the daemon issued the node no capability"
            time.sleep(0.2)

    def card_waiting(self, seen):
        queue, _ = self.phone.queue()
        return next((card for card in queue if card not in seen), None)

    def run(self, worktree, command, mode="default", answer=None):
        """One Claude Code session in `worktree` running `command`. Returns the card it raised (with
        its actions and text) or None when it did not ask; `answer` is the option chosen on it."""
        self.model.command = command
        session = str(uuid.uuid4())
        self.node(session, worktree)
        before = sum(1 for _ in open(self.asked_log)) if os.path.exists(self.asked_log) else 0
        seen = set(self.phone.queue()[0])
        env = {k: v for k, v in self.sandbox.env.items()}
        env.update(HOME=self.home, ANTHROPIC_BASE_URL=f"http://127.0.0.1:{self.model.server_address[1]}",
                   ANTHROPIC_API_KEY="sk-ant-e2e-local", ALONDRA_LAUNCHED="1", CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC="1",
                   DISABLE_AUTOUPDATER="1")
        claude = subprocess.Popen(["claude", "-p", "--session-id", session, "--permission-mode", mode, "--model", "claude-sonnet-4-5",
                                   "Run the command."], cwd=worktree, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        card = None
        deadline = time.time() + TIMEOUT * 2
        while claude.poll() is None:
            assert time.time() < deadline, f"{command!r}: Claude Code did not finish"
            if card is None and (waiting := self.card_waiting(seen)):
                actions, epoch, need = self.phone.actions(waiting)
                view = self.phone.view(f"card:{waiting}")
                card = {"id": waiting, "options": [a.get("option_id") for a in actions if a["id"].startswith("ans:")],
                        "actions": actions, "text": json.dumps(view, ensure_ascii=False), "need": need}
                chosen = next(a for a in actions if a["id"].startswith("ans:") and a.get("option_id") == (answer or "allow"))
                reply = self.phone.request("POST", "/actions", {"protocol": 1, "command_id": str(uuid.uuid4()), "data_epoch": epoch,
                                                                "client": "ios", "lang": "es", "action_id": chosen["id"],
                                                                "option_id": chosen["option_id"], "input": {}})
                assert reply["outcome"] == "applied", reply
            time.sleep(0.3)
        out, err = claude.communicate()
        assert claude.returncode == 0, f"{command!r}: claude failed: {out}\n{err}"
        asked = (sum(1 for _ in open(self.asked_log)) if os.path.exists(self.asked_log) else 0) > before
        assert asked == (card is not None), f"{command!r}: the hook ran ({asked}) but the card ({card}) did not match"
        return card

    def remove_from_settings(self):
        view = self.phone.view("machine")
        section = next((s for s in view["sections"] if s["id"] == "allowed_rules"), None)
        assert section, f"the machine view lists the rule: {json.dumps(view)[:2000]}"
        row = section["items"][0]
        action = next(a for a in row["actions"] if a["id"].startswith("remove_rule:"))
        reply = self.phone.request("POST", "/actions", {"protocol": 1, "command_id": str(uuid.uuid4()), "data_epoch": view["data_epoch"],
                                                        "client": "ios", "lang": "es", "action_id": action["id"], "input": {}})
        assert reply["outcome"] == "applied", reply
        return row

    def stop(self):
        for connection in self.connections:
            if connection.poll() is None:
                connection.kill()
                connection.wait()
        self.model.shutdown()
        self.sandbox.stop()


def ok(text):
    print(f"ok: {text}", file=sys.stderr, flush=True)


def main():
    signal.signal(signal.SIGTERM, lambda signum, _frame: sys.exit(128 + signum))
    if not shutil.which("claude"):
        sys.exit("claude is not on PATH")
    world = World(sys.argv[1:])
    try:
        w1, w2 = world.worktrees
        command = "make build 2>&1 | tail -1"

        card = world.run(w1, command, answer="always")
        assert card and card["options"] == ["allow", "always", "deny"], card
        assert "Permite esto ahora y en `app` deja de preguntar por" in card["text"], card["text"]
        assert world.rules() == [RULE], world.rules()
        assert not os.path.exists(os.path.join(w1, ".claude", "settings.local.json")), "the rule lives in the main checkout"
        assert world.run(w2, command) is None, "a new session in the other worktree does not ask"
        assert world.run(world.repo, command) is None, "nor does one in the main checkout"
        ok("1. allow always makes the same command stop asking, in every worktree of the repo")

        git(world.repo, "worktree", "remove", "--force", w1)
        assert world.run(w2, command) is None
        ok("2. the rule outlives the worktree it was accepted in")

        for destructive in ("rm -rf out", "make build; rm -rf out"):
            for _ in range(2):
                card = world.run(w2, destructive)
                assert card and card["options"] == ["allow", "deny"], card
                assert "No se puede permitir siempre" in card["text"], card["text"]
        ok("3. a destructive command offers no allow always and asks again after being allowed once")

        row = world.remove_from_settings()
        assert "make build" in row["title"], row
        assert world.rules() == [], world.rules()
        assert world.run(w2, command, answer="allow") is not None, "removed, the command asks again"
        ok("4. removing the rule from the settings makes the command ask again")

        before = open(world.settings_file).read()
        card = world.run(w2, "make build && rm -rf out")
        assert card and card["options"] == ["allow", "deny"], card
        assert open(world.settings_file).read() == before, "the rules file did not change"
        ok("5. Claude Code's own suggestion to allow `rm -rf out` is never offered")

        os.makedirs(os.path.join(w2, "out"), exist_ok=True)
        card = world.run(w2, "make build > out/log.txt")
        assert card and card["options"] == ["allow", "deny"], card
        card = world.run(w2, "make build > out/log.txt", mode="acceptEdits", answer="always")
        assert card and card["options"] == ["allow", "always", "deny"], card
        assert world.rules() == [RULE], world.rules()
        assert world.run(w2, "make build > out/log.txt", mode="acceptEdits") is None
        ok("6. a redirection offers allow always only where the write does not ask, and then stops asking")

        print("e2e allow always: every case behaved as the design says.")
    finally:
        world.stop()


if __name__ == "__main__":
    main()
