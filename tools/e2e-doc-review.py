#!/usr/bin/env python3
"""Reviews a document in the real TUI against the e2e sandbox and checks the comments arrive.

Usage: tools/e2e-doc-review.py <alondrad> <alondra> <alondra-relay> <alondra-sandbox>
(other outputs bazel lists for those targets, such as clippy's .ok files, are skipped)

Starts its own sandbox in a fresh directory. A node without a model (the real `alondra node
connect` and `alondra mcp-node`, as the plugin runs them) writes a document and asks for its
review. `alondra` runs in a pseudo-terminal: it opens the card, comments two blocks and sends
them. Each scenario checks both ends: the node was handed the comments, and the TUI says they
were sent instead of "no longer in the inbox". Scenarios:
  1. a plain review;
  2. the node asks again about a newer revision while the user writes the send: the TUI offers
     to send the comments to the new version;
  3. the card is replaced before the user sends: the comments go to the node as a message.
E2E_CAPTURES=<dir> keeps what the TUI showed at each step there; the sandbox and every other
file it made are removed at the end, whatever happens.
"""

import hashlib
import json
import os
import queue
import re
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import uuid

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TIMEOUT = 60
TITLE = "Migration design"
DOCUMENT = """# Migration design

Migrate the store in one go.

Remove the old endpoint after the release.
"""


class Terminal:
    """The TUI in its own tmux server, read with capture-pane as a person would see it."""

    def __init__(self, argv, env, captures):
        self.socket = f"alondra-review-{os.getpid()}"
        self.captures, self.shots = captures, 0
        command = " ".join(["env", "-i"] + [f"{k}={shlex.quote(v)}" for k, v in env.items()] + [shlex.quote(a) for a in argv])
        self.tmux("new-session", "-d", "-x", "200", "-y", "50", command)

    def tmux(self, *args):
        return subprocess.run(["tmux", "-L", self.socket, "-f", "/dev/null", *args], check=True, capture_output=True, text=True).stdout

    def screen(self):
        return self.tmux("capture-pane", "-p", "-t", "0")

    def expect(self, text, seconds=TIMEOUT):
        print(f"{time.strftime('%H:%M:%S')} waiting for {text!r}", file=sys.stderr, flush=True)
        deadline = time.time() + seconds
        while text not in (screen := self.screen()):
            if time.time() > deadline:
                raise AssertionError(f"never saw {text!r}; the screen:\n{screen}")
            time.sleep(0.3)
        return screen

    def capture(self, name):
        """Keeps what the user sees at this step, for the report."""
        self.shots += 1
        with open(os.path.join(self.captures, f"{self.shots:02d}-{name}.txt"), "w") as file:
            file.write(self.screen())

    def send(self, *keys):
        time.sleep(0.6)
        self.tmux("send-keys", "-t", "0", *keys)

    def type(self, text):
        time.sleep(0.6)
        self.tmux("send-keys", "-t", "0", "-l", text)

    def stop(self):
        subprocess.run(["tmux", "-L", self.socket, "kill-server"], capture_output=True)


class Node:
    """A node without a model: its connection to the daemon and its MCP tools."""

    def __init__(self, alondra, env):
        self.alondra, self.env = alondra, env
        self.session = str(uuid.uuid4())
        key = f"e2e-review-{self.session[:8]}"
        sessions = os.path.join(env["ALONDRA_HOME"], "runtime", "sessions")
        os.makedirs(sessions, exist_ok=True)
        self.settings = os.path.join(sessions, f"{key}.json")
        descriptor = os.open(self.settings, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(descriptor, "w") as file:
            json.dump({"env": {"ALONDRA_DOMAIN_AGENT": self.session}}, file)
        self.connection = subprocess.Popen([alondra, "node", "connect"], env=env, stdin=subprocess.PIPE,
                                           stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        self.connection.stdin.write(json.dumps({"session_id": self.session, "version": "2.1.287", "cwd": REPO, "launch_key": key}))
        self.connection.stdin.close()
        assert self.connection.stdout.readline() == "connected\n", "the node could not connect"
        self.lines = queue.Queue()
        threading.Thread(target=self._read, daemon=True).start()
        deadline = time.time() + TIMEOUT
        while "ALONDRA_DOMAIN_CAPABILITY" not in json.load(open(self.settings))["env"]:
            assert time.time() < deadline, "the daemon issued the node no capability"
            time.sleep(0.2)
        self.task = f"sandbox-probe-{self.session}"

    def _read(self):
        """Takes each message as the plugin does: hands it to the session, then acknowledges it."""
        for line in self.connection.stdout:
            message = json.loads(line)
            if "id" in message:
                subprocess.run([self.alondra, "node", "ack", message["id"]], env=self.env,
                               input=json.dumps({"session_id": self.session}), text=True, check=True, capture_output=True)
            self.lines.put(message)

    def call(self, *calls):
        env = dict(self.env, ALONDRA_DOMAIN_SETTINGS=self.settings, ALONDRA_DOMAIN_PROTOCOL="2")
        requests = [{"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}]
        requests += [{"jsonrpc": "2.0", "id": index + 1, "method": "tools/call", "params": {"name": tool, "arguments": arguments}}
                     for index, (tool, arguments) in enumerate(calls)]
        run = subprocess.run([self.alondra, "mcp-node"], env=env, input="\n".join(json.dumps(r) for r in requests) + "\n",
                             capture_output=True, text=True, timeout=TIMEOUT)
        replies = [json.loads(line) for line in run.stdout.splitlines() if line.strip()][1:]
        for (tool, _), reply in zip(calls, replies):
            result = reply.get("result", {})
            assert not result.get("isError"), f"{tool} was refused: {result}"
        assert len(replies) == len(calls), run.stdout + run.stderr

    def document(self, revision, text, first=False):
        name = f"e2e-review-{self.session[:8]}-{revision}.md"
        os.makedirs(os.path.join(self.env["ALONDRA_HOME"], "files"), exist_ok=True)
        with open(os.path.join(self.env["ALONDRA_HOME"], "files", name), "w") as file:
            file.write(text)
        digest = hashlib.sha256(text.encode()).hexdigest()
        if first:
            return ("create_document", {"task_id": self.task, "document_id": "design", "title": TITLE, "revision_id": revision,
                                        "content_digest": digest, "content_reference": f"files/{name}", "role": "design"})
        return ("revise_document", {"task_id": self.task, "document_id": "design", "revision_id": revision,
                                    "content_digest": digest, "content_reference": f"files/{name}"})

    def ask(self, interaction, revision, need, replaces=None):
        question = {"interaction_id": interaction, "scope": {"kind": "task", "task_id": self.task}, "recipient": {"kind": "user"},
                    "need": need, "why": "The migration waits for this review.", "blocking": True,
                    "options": [{"id": "approve", "label": "Approve", "description": "The migration starts", "inputs": []},
                                {"id": "drop", "label": "Drop it", "description": "Nothing is migrated", "inputs": []}]}
        if replaces:
            question["replaces"] = replaces
        return [("ask", question),
                ("link_interaction_subject", {"interaction_id": interaction,
                                              "subject": {"kind": "document_review", "document_id": "design", "revision_id": revision}})]

    def told(self, *expected):
        """The next message the daemon handed this node."""
        try:
            line = self.lines.get(timeout=TIMEOUT)
        except queue.Empty:
            raise AssertionError(f"the node was never handed {expected}")
        for text in expected:
            assert text in line["text"], f"the node was handed, without {text!r}:\n{line['text']}"
        print(f"node was handed:\n{line['text']}\n", file=sys.stderr, flush=True)
        return line["text"]

    def stop(self):
        if self.connection.poll() is None:
            self.connection.kill()
            self.connection.wait()


def comment_two_blocks(tui, first="Migrate the store in one go"):
    """Opens the selected card's document and comments its two paragraphs."""
    tui.send("Enter")
    tui.expect(first)
    for paragraph, comment in ((first, "Split this step in two"), ("Remove the old endpoint", "Keep the old endpoint a release longer")):
        tui.send("/")
        tui.type(paragraph)
        tui.send("Enter")
        tui.send("c")
        tui.type(comment)
        tui.send("Enter")
    tui.expect("2 comentarios sin enviar")
    tui.capture("commented")


def sent(tui, name):
    tui.expect("Comentarios enviados")
    tui.capture(name)
    for _ in range(10):
        screen = tui.screen()
        time.sleep(0.3)
        assert "ya no está en la bandeja" not in screen, f"the TUI said the card was gone after sending:\n{screen}"
        assert "sin enviar" not in screen, f"the comments still show as unsent:\n{screen}"


def main():
    signal.signal(signal.SIGTERM, lambda signum, _frame: sys.exit(128 + signum))
    binaries = [os.path.abspath(p) for p in sys.argv[1:] if not p.endswith(".ok")]
    daemon, cli, relay, tool = binaries
    scratch = tempfile.mkdtemp(prefix="alondra-review-")
    sandbox = os.path.join(scratch, "sandbox")
    tui = node = None
    try:
        env = dict(os.environ, SANDBOX_DIR=sandbox, SANDBOX_PORT=os.environ.get("SANDBOX_PORT", "47930"), MACS="1")
        subprocess.run([os.path.join(REPO, "tools/e2e-sandbox.sh"), daemon, cli, relay, tool], env=env, check=True, stdout=subprocess.DEVNULL)
        home = json.load(open(os.path.join(sandbox, "sandbox.json")))["macs"][0]["home"]
        child = {k: v for k, v in os.environ.items() if not k.startswith("ALONDRA_")}
        for line in open(os.path.join(home, "env")):
            key, _, value = line.removeprefix("export ").strip().partition("=")
            child[key] = value.strip('"')
        child.update(TERM="xterm-256color", ALONDRA_LANG="es", ALONDRA_SANDBOX="1")
        alondra = os.path.join(sandbox, "bin/alondra")
        node = Node(alondra, child)
        comments = ("Migrate the store in one go", "Split this step in two",
                    "Remove the old endpoint after the release", "Keep the old endpoint a release longer")

        # 1. A plain review: the comments reach the node and the TUI says they were sent.
        node.call(node.document("design-r1", DOCUMENT, first=True), *node.ask("review-1", "design-r1", "Review the migration design"))
        captures = os.environ.get("E2E_CAPTURES") or os.path.join(scratch, "captures")
        os.makedirs(captures, exist_ok=True)
        tui = Terminal([alondra], child, captures)
        tui.expect("Review the migration design")
        comment_two_blocks(tui)
        tui.send("s")
        tui.expect("Pedir cambios")
        tui.send("Enter")
        sent(tui, "sent")
        node.told(*comments)
        print("ok: comments sent from the TUI reach the node and the TUI says they were sent", file=sys.stderr, flush=True)

        # 2. While the user writes the send, the node asks again about a newer revision.
        node.call(*node.ask("review-2", "design-r1", "Review the migration design again"))
        tui.expect("Review the migration design again")
        comment_two_blocks(tui)
        tui.send("s")
        tui.expect("Pedir cambios")
        node.call(node.document("design-r2", DOCUMENT.replace("in one go", "in two steps")),
                  *node.ask("review-3", "design-r2", "Review version 2 of the migration design", replaces="review-2"))
        time.sleep(2)
        tui.send("Enter")
        tui.expect("tiene una versión nueva")
        tui.expect("Enviarlos a la versión nueva")
        tui.capture("newer-version-offered")
        tui.send("Enter")
        sent(tui, "sent-to-newer-version")
        node.told(*comments)
        print("ok: when the node moves to a newer revision mid-send, the TUI offers the new one and the comments arrive", file=sys.stderr, flush=True)

        # 3. The card is replaced before the user sends: the comments go to the node as a message.
        node.call(*node.ask("review-4", "design-r2", "Review the migration design once more"))
        tui.expect("Review the migration design once more")
        comment_two_blocks(tui, "Migrate the store in two steps")
        node.call(*node.ask("review-5", "design-r2", "Review the final migration design", replaces="review-4"))
        tui.expect("s: enviar tus comentarios a")
        tui.capture("card-gone")
        tui.send("s")
        sent(tui, "sent-to-node")
        node.told("Mis comentarios sobre «Review the migration design once more», sobre la versión que estaba leyendo", "Migrate the store in two steps", *comments[1:])
        print("ok: comments on a card that went away are sent to its node, never lost", file=sys.stderr, flush=True)
        if os.environ.get("E2E_CAPTURES"):
            print(f"TUI captures in {captures}", file=sys.stderr, flush=True)
        print("e2e doc review: the node got every comment and the TUI said they were sent, in all three cases.")
    except BaseException:
        if tui:
            print(f"the screen:\n{tui.screen()}", file=sys.stderr, flush=True)
        raise
    finally:
        if tui:
            tui.stop()
        if node:
            node.stop()
        print("stopping the sandbox", file=sys.stderr, flush=True)
        subprocess.run([os.path.join(REPO, "tools/e2e-sandbox-stop.sh"), sandbox], stdout=subprocess.DEVNULL, timeout=120)
        shutil.rmtree(scratch, ignore_errors=True)


if __name__ == "__main__":
    main()
