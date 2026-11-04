"""Cards of every kind in a throwaway e2e sandbox, for the card end-to-end checks.

`Sandbox` starts its own sandbox (tools/e2e-sandbox.sh) in a fresh directory and removes it, and
everything it made, on `stop()`. `Node` is a node without a model: the real `alondra node
connect` and `alondra mcp-node`, as the plugin runs them. `seed()` has it put one card of each
kind in front of the user:

  option     a decision answered with one of its options
  text       a request for a piece of text
  reply      a decision answered by writing in its reply field
  group      four questions asked together (group-q1 … group-q4)
  permission a tool permission the node waits on (the real `alondra hook`)
  delivery   a final result to accept
  orchestrator a Decide card the orchestrator asks, answered in its reply field: the words answer
             that card and reach the orchestrator with its id, never as a new request

Run it directly to keep a seeded sandbox up for a person or a simulator (SDK_NODE and SDK_HOST name
the pinned Node and the host bundle, as `make e2e-direct-cards` passes them):
  tools/e2e_cards.py <alondrad> <alondra> <alondra-relay> <alondra-sandbox>
"""

import argparse
import json
import os
import queue
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
import uuid

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TIMEOUT = 60

CARDS = {
    "option": "Pick the release channel",
    "text": "Write the release note",
    "reply": "Name the new settings screen",
    "action": "Rotate the signing key",
    "action-note": "Renew the push certificate",
    "group-q1": "Which store region goes first?",
    "group-q2": "Which price tier for the launch?",
    "group-q3": "Who signs the build?",
    "group-q4": "When does the beta close?",
    "permission": "git push",
    "delivery": "Review the migration report",
    "orchestrator": "Decide who fixes the memory pauses",
}

ORCHESTRATOR_ANSWER = "Do it yourself"
ORCHESTRATOR_CARD = {"interaction_id": "orchestrator-decide", "scope": {"kind": "orchestrator", "request_id": None},
                     "recipient": {"kind": "user"}, "title": CARDS["orchestrator"], "need": CARDS["orchestrator"], "why": "A node found a loop in the memory rule.",
                     "blocking": False, "options": [{"id": "mine", "label": "I'll do it", "description": "No task is created"},
                                                    {"id": "new_task", "label": "Create a task", "description": "A node fixes it"}]}


class Sandbox:
    def __init__(self, binaries, port, orchestrator="tools/e2e_fake_orchestrator.py", extra_env=None):
        named = {os.path.basename(p): os.path.abspath(p) for p in binaries if not p.endswith(".ok")}
        daemon, cli, relay, tool = (named[name] for name in ("alondrad", "alondra", "alondra-relay", "alondra-sandbox"))
        # The daemon opens its action journal without following a symlink, and /var is one on macOS;
        # its sockets also need a short path, which /private/var/folders/… is not.
        self.scratch = tempfile.mkdtemp(prefix="alondra-cards-", dir="/private/tmp")
        self.dir = os.path.join(self.scratch, "sandbox")
        fake = os.path.join(self.scratch, "fake-bin")
        os.makedirs(fake)
        os.symlink(os.path.join(REPO, orchestrator), os.path.join(fake, "claude"))
        self.orchestrator_log = os.path.join(self.scratch, "orchestrator.jsonl")
        env = dict(os.environ, SANDBOX_DIR=self.dir, SANDBOX_PORT=str(port), MACS="1", PATH=f"{fake}:{os.environ['PATH']}", SANDBOX_WAKE="1",
                   E2E_ORCHESTRATOR_LOG=self.orchestrator_log, E2E_ORCHESTRATOR_CARD=json.dumps(ORCHESTRATOR_CARD),
                   E2E_ALONDRA=os.path.join(self.dir, "bin/alondra"), **(extra_env or {}))
        try:
            subprocess.run([os.path.join(REPO, "tools/e2e-sandbox.sh"), daemon, cli, relay, tool], env=env, check=True,
                           stdout=subprocess.DEVNULL)
        except BaseException:
            self.stop()
            raise
        self.config = json.load(open(os.path.join(self.dir, "sandbox.json")))
        self.home = self.config["macs"][0]["home"]
        self.tool = tool
        self.env = {k: v for k, v in os.environ.items() if not k.startswith("ALONDRA_")}
        for line in open(os.path.join(self.home, "env")):
            key, _, value = line.removeprefix("export ").strip().partition("=")
            self.env[key] = value.strip('"')
        self.env.update(TERM="xterm-256color", ALONDRA_LANG="es", ALONDRA_SANDBOX="1")
        self.alondra = os.path.join(self.dir, "bin/alondra")

    def approve_pairing(self, seconds=TIMEOUT):
        """A person on the Mac taps Allow on the phone's pairing request, as the sandbox phone does."""
        local = f"http://127.0.0.1:{self.config['macs'][0]['port']}"
        deadline = time.time() + seconds
        while time.time() < deadline:
            with urllib.request.urlopen(f"{local}/devices/pending", timeout=5) as reply:
                pending = json.load(reply)["pending"]
            if pending:
                request = urllib.request.Request(f"{local}/devices/pending/{pending[0]['id']}/approve", data=b"{}", method="POST",
                                                 headers={"content-type": "application/json"})
                urllib.request.urlopen(request, timeout=5).close()
                return pending[0]
            time.sleep(0.5)
        raise AssertionError("no phone asked to pair")

    def stop(self):
        subprocess.run([os.path.join(REPO, "tools/e2e-sandbox-stop.sh"), self.dir], stdout=subprocess.DEVNULL, timeout=120)
        shutil.rmtree(self.scratch, ignore_errors=True)


class Node:
    """A node without a model: its connection to the daemon and its MCP tools."""

    def __init__(self, sandbox):
        self.alondra, self.env = sandbox.alondra, sandbox.env
        self.session = str(uuid.uuid4())
        self.workdir = tempfile.mkdtemp(prefix="node-", dir=sandbox.scratch)
        key = f"e2e-cards-{self.session[:8]}"
        sessions = os.path.join(self.env["ALONDRA_HOME"], "runtime", "sessions")
        os.makedirs(sessions, exist_ok=True)
        self.settings = os.path.join(sessions, f"{key}.json")
        descriptor = os.open(self.settings, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(descriptor, "w") as file:
            json.dump({"env": {"ALONDRA_DOMAIN_AGENT": self.session}}, file)
        self.connection = subprocess.Popen([self.alondra, "node", "connect"], env=self.env, stdin=subprocess.PIPE,
                                           stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        self.connection.stdin.write(json.dumps({"session_id": self.session, "version": "2.1.287", "cwd": self.workdir,
                                                "launch_key": key}))
        self.connection.stdin.close()
        assert self.connection.stdout.readline() == "connected\n", "the node could not connect"
        self.lines = queue.Queue()
        threading.Thread(target=self._read, daemon=True).start()
        deadline = time.time() + TIMEOUT
        while "ALONDRA_DOMAIN_CAPABILITY" not in json.load(open(self.settings))["env"]:
            assert time.time() < deadline, "the daemon issued the node no capability"
            time.sleep(0.2)
        self.task = f"sandbox-probe-{self.session}"
        self.holds = []

    def _read(self):
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
        return replies

    def scope(self):
        return {"kind": "task", "task_id": self.task}

    def ask(self, interaction, need, options):
        return ("ask", {"interaction_id": interaction, "scope": self.scope(), "recipient": {"kind": "user"}, "title": need, "need": need,
                        "why": "The release waits for this.", "blocking": False,
                        "options": [{"id": o, "label": o.capitalize(), "description": f"Go with {o}"} for o in options]})

    def action(self, interaction, need):
        """A request_action card with its default buttons, Done and I can't do it."""
        return ("request_action", {"interaction_id": interaction, "scope": self.scope(), "recipient": {"kind": "user"}, "title": need, "need": need,
                                   "why": "The release waits for this.", "verify": "I check it myself", "blocking": False,
                                   "steps": ["Open the developer console", "Do it"]})

    def heard(self, text):
        """The node got the user's words in the answer typed into its session."""
        deadline, seen = time.time() + TIMEOUT, []
        while time.time() < deadline:
            try:
                message = self.lines.get(timeout=1)
            except queue.Empty:
                continue
            seen.append(message)
            if text in json.dumps(message, ensure_ascii=False):
                return
        raise AssertionError(f"the node never got {text!r}: {seen}")

    def permission(self, command):
        """The real hook a session runs before a tool: it waits until the user answers."""
        payload = json.dumps({"session_id": self.session, "hook_event_name": "PermissionRequest", "permission_mode": "default",
                              "tool_name": "Bash", "tool_input": {"command": command}, "cwd": self.workdir})
        env = dict(self.env, ALONDRA_LAUNCHED="1")
        hold = subprocess.Popen([self.alondra, "hook"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, text=True)
        hold.stdin.write(payload)
        hold.stdin.close()
        self.holds.append(hold)
        return hold

    def stop(self):
        for hold in self.holds:
            if hold.poll() is None:
                hold.kill()
        if self.connection.poll() is None:
            self.connection.kill()
            self.connection.wait()


def seed(asker, deliverer):
    """One card of each kind, ids as in CARDS. A node delivers only once its questions are
    answered, so the delivery comes from a second node."""
    asker.call(
        asker.ask("option", CARDS["option"], ["stable", "beta"]),
        ("request_input", {"interaction_id": "text", "scope": asker.scope(), "recipient": {"kind": "user"}, "title": CARDS["text"], "need": CARDS["text"],
                           "why": "The release notes need it.", "kind": "text", "blocking": False}),
        asker.ask("reply", CARDS["reply"], ["settings", "preferences"]),
        asker.action("action", CARDS["action"]),
        asker.action("action-note", CARDS["action-note"]),
        ("ask", {"interaction_id": "group", "scope": asker.scope(), "recipient": {"kind": "user"}, "why": "", "blocking": False,
                 "questions": [{"question": CARDS[f"group-q{n}"], "why": "The launch plan needs it.",
                                "options": [{"id": "a", "label": "A", "description": "First choice"},
                                            {"id": "b", "label": "B", "description": "Second choice"}]}
                               for n in range(1, 5)]}),
    )
    deliverer.call(
        ("present_result", {"task_id": deliverer.task, "interaction_id": "delivery", "title": CARDS["delivery"], "need": CARDS["delivery"],
                            "content": {"kind": "report", "body": "The store moved in one go."},
                            "targets": [{"kind": "text", "text": "The store moved in one go."}], "is_final": True}),
    )
    asker.permission(CARDS["permission"])


def seed_orchestrator(sandbox, node):
    """A node's question to the orchestrator wakes it, and it asks the user its Decide card: the
    sandbox's fake orchestrator in the Mac's Agent SDK host (tools/sandbox/src/fake/orchestrator.rs)."""
    fake = os.path.join(sandbox.dir, "fakes", "orchestrator")
    os.makedirs(fake, exist_ok=True)
    with open(os.path.join(fake, "ask.json"), "w") as card:
        json.dump(ORCHESTRATOR_CARD, card)
    node.call(("ask", {"interaction_id": "to-orchestrator", "scope": node.scope(), "recipient": {"kind": "orchestrator"},
                       "title": "Tell me who fixes the memory pauses", "need": "Tell me who fixes the memory pauses", "why": "My build is cut in a loop.", "blocking": False,
                       "options": [{"id": "wait", "label": "Wait", "description": "Keep waiting"},
                                   {"id": "go", "label": "Go on", "description": "Go on without it"}]}))
    deadline = time.time() + TIMEOUT
    while not os.path.exists(os.path.join(fake, "asked.json")):
        if time.time() > deadline:
            log = "".join(open(os.path.join(sandbox.home, "alondrad.log")).readlines()[-40:])
            raise AssertionError(f"the orchestrator never asked its card:\n{log}")
        time.sleep(0.5)


def check_orchestrator_answered(sandbox):
    """The words written under the orchestrator's card reached it as the answer to that card."""
    deadline = time.time() + TIMEOUT
    while True:
        prompts = [json.loads(line)["prompt"] for line in open(os.path.join(sandbox.dir, "fakes", "orchestrator", "batches.jsonl"))]
        if any("(card orchestrator-decide" in prompt and ORCHESTRATOR_ANSWER in prompt for prompt in prompts):
            break
        assert time.time() < deadline, f"the orchestrator was never told the answer to its card: {prompts}"
        time.sleep(0.5)
    loose = subprocess.run(["sqlite3", "-readonly", os.path.join(sandbox.home, "domain.db"),
                            f"select count(*) from requests where body = '{ORCHESTRATOR_ANSWER}'"],
                           capture_output=True, text=True, check=True).stdout.strip()
    assert loose == "0", f"the answer also became {loose} new request(s)"
    print("ok: the orchestrator got the answer to its card, with its id, and no new request", file=sys.stderr, flush=True)


ANSWERS = {
    "option": ("ans:", "stable", None),
    "text": ("reply:", None, "Faster sync and a new settings screen"),
    "reply": ("reply:", None, "Call it Preferences"),
    "action": ("ans:", "done", None),
    "action-note": ("ans:", "cannot", "No tengo acceso a Apple Developer"),
    "group-q1": ("ans:", "a", None),
    "group-q2": ("ans:", "b", None),
    "group-q3": ("reply:", None, "Alice signs it"),
    "group-q4": ("ans:", "a", None),
    "permission": ("ans:", "allow", None),
    "delivery": ("accept:", None, None),
    "orchestrator": ("reply:", None, ORCHESTRATOR_ANSWER),
}


def kind_of(need):
    """The seeded card with this text, or None for one the sandbox raised by itself (a memory warning)."""
    return next((card for card, text in CARDS.items() if opening(text) in need), None)


class Phone:
    """The synthetic phone: every request encrypted, through the direct listener."""

    def __init__(self, sandbox):
        self.sandbox = sandbox
        self.env = dict(os.environ, SANDBOX_DIR=sandbox.dir)
        self.run("pair", "--direct")

    def run(self, *args):
        done = subprocess.run([self.sandbox.tool, "phone", "--name", "e2e-direct", "--mac", "1", *args],
                              env=self.env, capture_output=True, text=True, timeout=120)
        assert done.returncode == 0, done.stderr
        return done.stdout

    def request(self, method, path, body=None):
        args = ["request", "--direct", method, path, *([json.dumps(body)] if body else [])]
        reply = json.loads(self.run(*args))["response"]
        assert reply["status"] == 200, reply
        return reply["body"]

    def view(self, key):
        return self.request("GET", f"/views/{key}?protocol=1&client=ios&lang=es")

    def queue(self):
        home = self.view("home")
        return next((s.get("card_ids", []) for s in home["sections"] if s["id"] == "queue"), []), home

    def actions(self, card):
        view = self.view(f"card:{card}")
        assert view["key"] == f"card:{card}", f"the screen of {card} loads under its own key: {view['key']}"
        found, need = [], ""
        for section in view["sections"]:
            for item in section["items"]:
                found += item.get("actions") or []
                found += item.get("more") or []
                if isinstance(item.get("reply"), dict):
                    found.append(item["reply"])
                need = need or item.get("need") or item.get("title") or ""
        return found, view["data_epoch"], need


def check_phone(phone):
    """Every card answered from the phone leaves the queue at once, as its action promised."""
    route = "direct"
    queue, _ = phone.queue()
    seeded = {card: kind_of(phone.actions(card)[2]) for card in queue}
    seeded = {card: kind for card, kind in seeded.items() if kind}
    assert sorted(seeded.values()) == sorted(CARDS), f"every seeded card waits: {seeded}"
    for card in [c for c, kind in seeded.items() if kind.startswith("group-q")]:
        actions, _, _ = phone.actions(card)
        assert actions, f"{card} opens on its own screen with its answers"
        print(f"ok ({route}): the screen of {card} loads", file=sys.stderr, flush=True)
    for card, kind in seeded.items():
        prefix, option, text = ANSWERS[kind]
        actions, epoch, _ = phone.actions(card)
        action = next(a for a in actions if a["id"].startswith(prefix))
        assert action.get("resolves_card") is True, f"{card}: {prefix} says it takes its card out: {action}"
        before, home = phone.queue()
        reply = phone.request("POST", "/actions", {"protocol": 1, "command_id": str(uuid.uuid4()), "data_epoch": epoch,
                                                   "client": "ios", "lang": "es", "action_id": action["id"],
                                                   "option_id": option, "input": {"text": text}})
        assert reply["outcome"] == "applied", f"{card}: {reply}"
        changes = phone.request("GET", f"/changes?protocol=1&client=ios&lang=es&wait=0&cursor={home['cursor']}&views=home%40{home['version']}")
        assert "home" in changes["stale"], f"{card}: the phone's next poll says home changed: {changes}"
        for _ in range(3):
            assert card not in phone.queue()[0], f"{card} came back to the queue"
        print(f"ok ({route}): {card} left the queue once answered", file=sys.stderr, flush=True)
    left = [card for card in phone.queue()[0] if card in seeded]
    assert not left, f"answered cards still wait: {left}"


class Terminal:
    """The TUI in its own tmux server, read with capture-pane as a person would see it."""

    def __init__(self, argv, env, captures):
        self.socket = f"alondra-cards-{os.getpid()}"
        self.captures, self.shots = captures, 0
        command = " ".join(["env", "-i"] + [f"{k}={shlex.quote(v)}" for k, v in env.items()] + [shlex.quote(a) for a in argv])
        self.tmux("new-session", "-d", "-x", "160", "-y", "45", command)

    def tmux(self, *args):
        return subprocess.run(["tmux", "-L", self.socket, "-f", "/dev/null", *args], check=True, capture_output=True, text=True).stdout

    def screen(self):
        return self.tmux("capture-pane", "-p", "-t", "0")

    def until(self, predicate, what, seconds=TIMEOUT):
        deadline = time.time() + seconds
        while not predicate(screen := self.screen()):
            if time.time() > deadline:
                raise AssertionError(f"{what}; the screen:\n{screen}")
            time.sleep(0.2)
        return screen

    def capture(self, name):
        if not self.captures:
            return
        self.shots += 1
        with open(os.path.join(self.captures, f"{self.shots:02d}-{name}.txt"), "w") as file:
            file.write(self.screen())

    def keys(self, *keys):
        time.sleep(0.4)
        self.tmux("send-keys", "-t", "0", *keys)

    def type(self, text):
        time.sleep(0.4)
        self.tmux("send-keys", "-t", "0", "-l", text)

    def stop(self):
        subprocess.run(["tmux", "-L", self.socket, "kill-server"], capture_output=True)


def opening(need):
    """The first words of a card's title: the TUI's list wraps a long one after them."""
    return " ".join(need.split()[:4])


def listed(screen, need):
    """Whether the inbox list on the left still shows the card."""
    return any(opening(need) in line[:56] for line in screen.splitlines())


def check_tui(sandbox, captures):
    """Each card answered in the real TUI leaves its list at once and does not come back."""
    tui = Terminal([sandbox.alondra], sandbox.env, captures)
    try:
        tui.until(lambda s: "TE ESPERA" in s, "the inbox never showed")
        tui.capture("inbox")
        skipped = 0
        while True:
            screen = tui.screen()
            selected = next((line for line in screen.splitlines() if line.startswith("│▌")), None)
            if selected is None or not any(listed(screen, need) for need in CARDS.values()):
                break
            card = kind_of(selected)
            if card is None:
                skipped += 1
                assert skipped < 5, f"only cards the sandbox raised by itself are selected:\n{screen}"
                tui.keys("j")
                continue
            prefix, option, text = ANSWERS[card]
            need = CARDS[card]
            if card == "option":
                tui.keys("m")
                menu = tui.until(lambda s: "Pedir aclaración" in s, "the More menu never offered a clarification")
                clarify = next(line.split("Pedir aclaración")[0].split()[-1] for line in menu.splitlines() if "Pedir aclaración" in line)
                tui.keys(clarify)
                tui.keys("1")
                tui.type("Which release?")
                tui.keys("Enter")
                time.sleep(3)
                assert listed(tui.screen(), need), f"asking to clarify hid {card}, which still waits:\n{tui.screen()}"
                tui.capture("clarified-option-stays")
                print("ok (tui): asking to clarify keeps the card in the list", file=sys.stderr, flush=True)
            key = {"stable": "1", "a": "1", "b": "2", "allow": "1", "done": "1", "cannot": "2"}.get(option)
            if prefix == "reply:":
                tui.keys("r")
                tui.type(text)
                tui.keys("Enter")
            elif prefix == "accept:":
                tui.keys("1")
            elif text:
                tui.keys("o")
                tui.until(lambda s: "comentario general" in s, f"{card}: the reader did not open")
                tui.keys("C")
                tui.type(text)
                tui.keys("Enter")
                tui.capture(f"commented-{card}")
                tui.keys(key)
            else:
                tui.keys(key)
            tui.until(lambda s: not listed(s, need), f"{card} was still listed right after it was answered", seconds=2)
            tui.capture(f"answered-{card}")
            print(f"ok (tui): {card} left the list at once", file=sys.stderr, flush=True)
        time.sleep(17)
        screen = tui.screen()
        for card, need in CARDS.items():
            assert not listed(screen, need), f"{card} came back after the TUI stopped hiding it:\n{screen}"
        tui.capture("after-grace")
        print("ok (tui): no card came back once the TUI's own grace ran out", file=sys.stderr, flush=True)
    except BaseException:
        print(f"the screen:\n{tui.screen()}", file=sys.stderr, flush=True)
        raise
    finally:
        tui.stop()


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--client", choices=["hold", "direct", "tui", "ios"], default="hold")
    parser.add_argument("--captures", help="keep what each client showed at each step here")
    parser.add_argument("binaries", nargs="+")
    args = parser.parse_args()
    signal.signal(signal.SIGTERM, lambda signum, _frame: sys.exit(128 + signum))
    if not (os.environ.get("SDK_NODE") and os.environ.get("SDK_HOST")):
        parser.error("SDK_NODE and SDK_HOST must name the pinned Node and the host bundle: the orchestrator runs through the Agent SDK host")
    lan = args.client == "ios"
    if lan:
        os.environ["SANDBOX_LAN"] = "1"
    sandbox = Sandbox(args.binaries, int(os.environ.get("SANDBOX_PORT", "47950")), extra_env={"FAKES": "1"})
    nodes = []
    try:
        nodes = [Node(sandbox), Node(sandbox)]
        seed(*nodes)
        seed_orchestrator(sandbox, nodes[1])
        if args.captures:
            os.makedirs(args.captures, exist_ok=True)
        if args.client == "direct":
            check_phone(Phone(sandbox))
        elif args.client == "tui":
            check_tui(sandbox, args.captures)
        elif args.client == "ios":
            import e2e_ios_cards
            e2e_ios_cards.check(sandbox, ANSWERS, kind_of, args.captures)
        else:
            print(json.dumps({"sandbox": sandbox.dir, "home": sandbox.home, "config": sandbox.config}, indent=2), flush=True)
            print("seeded; Ctrl+C stops the sandbox", file=sys.stderr, flush=True)
            while True:
                time.sleep(3600)
        if args.client != "hold":
            check_orchestrator_answered(sandbox)
            nodes[0].heard(ANSWERS["action-note"][2])
            print(f"ok ({args.client}): the words written on the card reached the node with the option", file=sys.stderr, flush=True)
        print(f"e2e cards ({args.client}): every card left at once once answered and none came back.")
    except KeyboardInterrupt:
        pass
    finally:
        for node in nodes:
            node.stop()
        sandbox.stop()


if __name__ == "__main__":
    main()
