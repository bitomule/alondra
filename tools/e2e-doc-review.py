#!/usr/bin/env python3
"""Reviews a document in the real TUI against the e2e sandbox and checks the comments arrive.

Usage: tools/e2e-doc-review.py <alondrad> <alondra> <alondra-relay> <alondra-sandbox>
(other outputs bazel lists for those targets, such as clippy's .ok files, are skipped)

Starts its own sandbox in a fresh directory, with the fakes and the Agent SDK backend (SDK_NODE and
SDK_HOST name the pinned Node and the host bundle). The phone asks for the work; the node Alondra
starts for it is the sandbox's scripted native Claude in an SDK host, which, told by this test,
writes a document and asks for its review through the real `alondra mcp-node`. `alondra` runs in
a pseudo-terminal: it opens the card, comments two blocks and sends
them. Each scenario checks both ends: the node was handed the comments, and the TUI says they
were sent instead of "no longer in the inbox". Scenarios:
  1. a plain review;
  2. the node asks again about a newer revision while the user writes the send: the TUI offers
     to send the comments to the new version;
  3. the card is replaced before the user sends: the comments go to the node as a message.
E2E_CAPTURES=<dir> keeps what the TUI showed at each step there; the sandbox and every other
file it made are removed at the end, whatever happens.
"""

import glob
import hashlib
import json
import os
import re
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time

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
    """The task's node: the sandbox's scripted native Claude in the Mac's Agent SDK host. This test
    tells it what to call (`alondra-sandbox fake node`), and it calls through the real `alondra
    mcp-node` inside its own turn; what Alondra hands it is in its session's received inputs."""

    REQUEST = "req-review"

    def __init__(self, sandbox, env):
        self.sandbox, self.env = sandbox, env
        self.task = "t-review"
        scripts = os.path.join(sandbox, "fakes", "scripts")
        os.makedirs(scripts, exist_ok=True)
        with open(os.path.join(scripts, f"{self.REQUEST}.json"), "w") as file:
            json.dump({"request_id": self.REQUEST, "tasks": [{"task_id": self.task, "title": TITLE}]}, file)
        self.tool("phone", "--mac", "1", "pair", "--direct")
        self.tool("phone", "--mac", "1", "request", "--direct", "POST", "/requests",
                  json.dumps({"request_id": self.REQUEST, "text": "Write the migration design and ask for its review"}))
        deadline = time.time() + TIMEOUT
        while not (session := self.session()) or not self.received(session["short"]):
            assert time.time() < deadline, "Alondra started no node for the task through the SDK host"
            time.sleep(0.2)
        self.short = session["short"]
        self.seen = len(self.received(self.short))

    def tool(self, *args):
        run = subprocess.run([os.path.join(self.sandbox, "bin/alondra-sandbox"), "--sandbox", self.sandbox, *args], env=self.env,
                             capture_output=True, text=True, timeout=TIMEOUT * 3)
        assert run.returncode == 0, f"alondra-sandbox {' '.join(args[:3])} failed: {run.stdout}{run.stderr}"
        return run.stdout

    def session(self):
        """The task's running session, as the fake native Claude writes it down."""
        for path in sorted(glob.glob(os.path.join(self.sandbox, "fakes", "sessions", "*", "session.json"))):
            session = json.load(open(path))
            if session.get("task_id") == self.task and session.get("state") == "running":
                return session
        return None

    def received(self, short):
        try:
            return [json.loads(line) for line in open(os.path.join(self.sandbox, "fakes", "sessions", short, "received.jsonl")) if line.strip()]
        except FileNotFoundError:
            return []

    def call(self, *calls):
        steps = [{"do": "call", "tool": tool, "arguments": arguments} for tool, arguments in calls]
        lines = [line for line in self.tool("fake", "node", self.task, "call", "--step", json.dumps(steps)).splitlines() if line.strip()]
        assert len(lines) == len(calls) and all(line.startswith("ok ") for line in lines), "\n".join(lines)

    def document(self, revision, text, first=False):
        name = f"e2e-review-{self.short}-{revision}.md"
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
                    "options": [{"id": "approve", "label": "Approve", "description": "The migration starts"},
                                {"id": "drop", "label": "Drop it", "description": "Nothing is migrated"}]}
        if replaces:
            question["replaces"] = replaces
        return [("ask", question),
                ("link_interaction_subject", {"interaction_id": interaction,
                                              "subject": {"kind": "document_review", "document_id": "design", "revision_id": revision}})]

    def told(self, *expected):
        """The next input Alondra handed this node."""
        deadline = time.time() + TIMEOUT
        while len(received := self.received(self.short)) <= self.seen:
            assert time.time() < deadline, f"the node was never handed {expected}"
            time.sleep(0.2)
        text = received[self.seen]["text"]
        self.seen += 1
        for wanted in expected:
            assert wanted in text, f"the node was handed, without {wanted!r}:\n{text}"
        print(f"node was handed:\n{text}\n", file=sys.stderr, flush=True)
        return text


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
    # The node's order socket needs a short path.
    scratch = os.path.realpath(tempfile.mkdtemp(prefix="alondra-review-", dir="/private/tmp"))
    sandbox = os.path.join(scratch, "sandbox")
    tui = None
    try:
        env = dict(os.environ, SANDBOX_DIR=sandbox, SANDBOX_PORT=os.environ.get("SANDBOX_PORT", "47930"), MACS="1", FAKES="1")
        subprocess.run([os.path.join(REPO, "tools/e2e-sandbox.sh"), daemon, cli, relay, tool], env=env, check=True, stdout=subprocess.DEVNULL)
        home = json.load(open(os.path.join(sandbox, "sandbox.json")))["macs"][0]["home"]
        child = {k: v for k, v in os.environ.items() if not k.startswith("ALONDRA_")}
        for line in open(os.path.join(home, "env")):
            key, _, value = line.removeprefix("export ").strip().partition("=")
            child[key] = value.strip('"')
        child.update(TERM="xterm-256color", ALONDRA_LANG="es", ALONDRA_SANDBOX="1")
        alondra = os.path.join(sandbox, "bin/alondra")
        node = Node(sandbox, child)
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
        tui.capture("send-open")
        tui.send("Enter")
        tui.capture("submitted")
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
        print("stopping the sandbox", file=sys.stderr, flush=True)
        subprocess.run([os.path.join(REPO, "tools/e2e-sandbox-stop.sh"), sandbox], stdout=subprocess.DEVNULL, timeout=120)
        shutil.rmtree(scratch, ignore_errors=True)


if __name__ == "__main__":
    main()
