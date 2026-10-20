#!/usr/bin/env python3
"""«Allow always» end to end through the Agent SDK host, against its own throwaway sandbox.

Each run is a task of the sandbox's repository, whose node is the sandbox's scripted native Claude
(tools/sandbox/src/fake/native.rs) in the Mac's SDK host, in the worktree Alondra made for the
task. Told by this test to run one Bash command, it asks the host for its permission as Claude Code
does in default mode, unless the `.claude/settings.local.json` of the repository's main checkout
allows it, where Claude Code reads the rules of every worktree. The card is answered from the
sandbox phone, as the apps do; «Allow always» has Alondra keep the rule there. No model, no
account, no tokens. Nothing touches the real ~/.claude, ~/.claude.json or ~/.alondra; everything
is removed at the end.

  tools/e2e_allow_always.py <alondrad> <alondra> <alondra-relay> <alondra-sandbox>
  (SDK_NODE and SDK_HOST name the pinned Node and the host bundle)
"""

import glob
import json
import os
import shutil
import signal
import subprocess
import sys
import time
import uuid

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from e2e_cards import TIMEOUT, Phone, Sandbox  # noqa: E402

RULE = "Bash(make build *)"


def git(cwd, *args):
    subprocess.run(["git", "-C", cwd, "-c", "user.name=e2e", "-c", "user.email=e2e@example.com", "-c", "commit.gpgsign=false", *args],
                   check=True, capture_output=True)


class World:
    """The sandbox with its fakes, a Makefile on the main branch of its repository, and the phone."""

    def __init__(self, binaries):
        self.sandbox = Sandbox(binaries, int(os.environ.get("SANDBOX_PORT", "48050")), extra_env={"FAKES": "1"})
        self.repo = os.path.realpath(os.path.join(self.sandbox.dir, "repo"))
        with open(os.path.join(self.repo, "Makefile"), "w") as makefile:
            makefile.write("build:\n\t@echo built\ntest:\n\t@echo tested\n")
        git(self.repo, "add", "Makefile")
        git(self.repo, "commit", "-q", "-m", "A Makefile for the cases")
        git(self.repo, "push", "-q", "origin", "HEAD")
        self.phone = Phone(self.sandbox)
        self.tasks = 0

    @property
    def settings_file(self):
        return os.path.join(self.repo, ".claude", "settings.local.json")

    def rules(self):
        try:
            return json.load(open(self.settings_file)).get("permissions", {}).get("allow", [])
        except FileNotFoundError:
            return []

    def tool(self, *args):
        return subprocess.Popen([self.sandbox.tool, "--sandbox", self.sandbox.dir, *args], env=dict(os.environ, SANDBOX_DIR=self.sandbox.dir),
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)

    def session(self, task):
        for path in glob.glob(os.path.join(self.sandbox.dir, "fakes", "sessions", "*", "session.json")):
            session = json.load(open(path))
            if session.get("task_id") == task and session.get("state") == "running":
                return session
        return None

    def lines(self, session, name):
        try:
            return [json.loads(line) for line in open(os.path.join(self.sandbox.dir, "fakes", "sessions", session["short"], name)) if line.strip()]
        except FileNotFoundError:
            return []

    def node(self):
        """A new task, and the session of its node once its native Claude runs and took its mission."""
        self.tasks += 1
        task, request = f"t-always-{self.tasks}", f"req-always-{self.tasks}"
        scripts = os.path.join(self.sandbox.dir, "fakes", "scripts")
        os.makedirs(scripts, exist_ok=True)
        with open(os.path.join(scripts, f"{request}.json"), "w") as script:
            json.dump({"request_id": request, "tasks": [{"task_id": task, "title": f"Run case command {self.tasks}"}]}, script)
        self.phone.request("POST", "/requests", {"request_id": request, "text": f"Run case command {self.tasks}"})
        deadline = time.time() + TIMEOUT
        while not (session := self.session(task)) or not self.lines(session, "received.jsonl"):
            assert time.time() < deadline, f"Alondra started no node for {task}"
            time.sleep(0.2)
        return task, session

    def card_waiting(self, seen):
        queue, _ = self.phone.queue()
        return next((card for card in queue if card not in seen), None)

    def run(self, command, answer=None, node=None):
        """`command` run by a new node, or by `node`. Returns the card it raised (with its actions
        and text) or None when it did not ask; `answer` is the option chosen on it."""
        task, session = node or self.node()
        seen = set(self.phone.queue()[0])
        before = sum(1 for permission in self.lines(session, "permissions.jsonl") if permission["asked"])
        running = self.tool("fake", "node", task, "bash", "--step", json.dumps({"do": "bash", "command": command}))
        card = None
        deadline = time.time() + TIMEOUT * 2
        while running.poll() is None:
            assert time.time() < deadline, f"{command!r}: the node did not finish"
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
        out, err = running.communicate()
        assert running.returncode == 0 and out.startswith("ok "), f"{command!r}: the node failed: {out}\n{err}"
        asked = sum(1 for permission in self.lines(session, "permissions.jsonl") if permission["asked"]) > before
        assert asked == (card is not None), f"{command!r}: the node asked ({asked}) but the card ({card}) did not match"
        if node is None:
            self.cancel(task)
        return card

    def cancel(self, task):
        """The user cancels the task, so its node leaves a slot for the next one."""
        self.phone.run("press", "--direct", f"task:{task}", "cancel:")
        deadline = time.time() + TIMEOUT
        while self.session(task):
            assert time.time() < deadline, f"the node of {task} did not end"
            time.sleep(0.2)

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
        self.sandbox.stop()


def ok(text):
    print(f"ok: {text}", file=sys.stderr, flush=True)


def main():
    signal.signal(signal.SIGTERM, lambda signum, _frame: sys.exit(128 + signum))
    world = World(sys.argv[1:])
    try:
        command = "make build 2>&1 | tail -1"

        first = world.node()
        card = world.run(command, answer="always", node=first)
        assert card and card["options"] == ["allow", "always", "deny"], card
        assert "Permite esto ahora y en `repo` deja de preguntar por" in card["text"], card["text"]
        assert world.rules() == [RULE], world.rules()
        worktree = first[1]["cwd"]
        assert not os.path.exists(os.path.join(worktree, ".claude", "settings.local.json")), "the rule lives in the main checkout"
        assert world.run(command) is None, "a new node in another worktree does not ask"
        ok("1. allow always makes the same command stop asking, in every worktree of the repo")

        world.cancel(first[0])
        if os.path.exists(worktree):
            git(world.repo, "worktree", "remove", "--force", worktree)
        assert world.run(command) is None
        ok("2. the rule outlives the worktree it was accepted in")

        for destructive in ("rm -rf out", "make build; rm -rf out"):
            for _ in range(2):
                card = world.run(destructive)
                assert card and card["options"] == ["allow", "deny"], card
                assert "No se puede permitir siempre" in card["text"], card["text"]
        ok("3. a destructive command offers no allow always and asks again after being allowed once")

        row = world.remove_from_settings()
        assert "make build" in row["title"], row
        assert world.rules() == [], world.rules()
        assert world.run(command, answer="allow") is not None, "removed, the command asks again"
        ok("4. removing the rule from the settings makes the command ask again")

        before = open(world.settings_file).read()
        card = world.run("make build && rm -rf out")
        assert card and card["options"] == ["allow", "deny"], card
        assert open(world.settings_file).read() == before, "the rules file did not change"
        ok("5. Claude Code's own suggestion to allow `rm -rf out` is never offered")

        writer = world.node()
        os.makedirs(os.path.join(writer[1]["cwd"], "out"), exist_ok=True)
        card = world.run("make build > out/log.txt", node=writer)
        assert card and card["options"] == ["allow", "deny"], card
        shutil.rmtree(os.path.join(writer[1]["cwd"], "out"))
        world.cancel(writer[0])
        ok("6. a command that writes a file through a redirection offers no allow always")

        print("e2e allow always: every case behaved as the design says.")
    finally:
        world.stop()


if __name__ == "__main__":
    main()
