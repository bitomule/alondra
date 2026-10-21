#!/usr/bin/env python3
"""A turn that ends on a message to the orchestrator, seen in the real TUI in tmux.

The task's node is the sandbox's scripted native Claude in the Mac's Agent SDK host. It tells the
orchestrator, without blocking, to create one task per phase, and its turn ends there. The task
must read «Esperando al orquestador» in the TUI and on the phone, with no card for the user and
nothing «Te necesita». No model, no account, no tokens; everything is removed at the end.

  tools/e2e_turn_end_wait.py [--captures DIR] <alondrad> <alondra> <alondra-relay> <alondra-sandbox>
  (SDK_NODE and SDK_HOST name the pinned Node and the host bundle)
"""

import argparse
import glob
import json
import os
import signal
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from e2e_cards import TIMEOUT, Phone, Sandbox, Terminal  # noqa: E402

TASK, REQUEST = "t-phases", "req-phases"
WAITING = "Esperando al orquestador"
CLAIMS = ("Te necesita", "sin entregar ni preguntar")
# The card tools/e2e_fake_orchestrator.py asks the user on its first run, about nothing of this task.
FAKE_ORCHESTRATOR_CARD = "orchestrator-decide"
# That fake never answers the node, so Alondra tells the user the orchestrator left its question
# unanswered: the orchestrator failing them, which is theirs to hear.
UNANSWERED = "problem-unanswered-question-"


def session(sandbox):
    for path in glob.glob(os.path.join(sandbox.dir, "fakes", "sessions", "*", "session.json")):
        found = json.load(open(path))
        if found.get("task_id") == TASK and found.get("state") == "running":
            return found
    return None


def start_node(sandbox, phone):
    scripts = os.path.join(sandbox.dir, "fakes", "scripts")
    os.makedirs(scripts, exist_ok=True)
    with open(os.path.join(scripts, f"{REQUEST}.json"), "w") as script:
        json.dump({"request_id": REQUEST, "tasks": [{"task_id": TASK, "title": "Remove the Fleet"}]}, script)
    phone.request("POST", "/requests", {"request_id": REQUEST, "text": "Remove the Fleet"})
    deadline = time.time() + TIMEOUT
    while not session(sandbox):
        assert time.time() < deadline, f"Alondra started no node for {TASK}"
        time.sleep(0.2)


def message_the_orchestrator(sandbox):
    step = {"do": "call", "tool": "ask", "arguments": {
        "interaction_id": "t-phases-1", "scope": {"kind": "task", "task_id": TASK}, "recipient": {"kind": "orchestrator"},
        "need": "Create one task per phase 2-7", "why": "The user approved a plan in phases", "blocking": False,
        "body": "The plan is approved: create one task per phase 2-7.",
        "options": [{"id": "create", "label": "Create them", "description": "One task per phase"},
                    {"id": "decline", "label": "Do not create them", "description": "This task does every phase"}]}}
    run = subprocess.run([sandbox.tool, "--sandbox", sandbox.dir, "fake", "node", TASK, "call", "--step", json.dumps(step)],
                         env=dict(os.environ, SANDBOX_DIR=sandbox.dir), capture_output=True, text=True, timeout=TIMEOUT * 2)
    assert run.returncode == 0 and run.stdout.startswith("ok "), f"the node could not message the orchestrator: {run.stdout}\n{run.stderr}"


def task_says(phone):
    return json.dumps(phone.view(f"task:{TASK}"), ensure_ascii=False)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--captures", help="keep what the TUI showed at each step here")
    parser.add_argument("binaries", nargs="+")
    args = parser.parse_args()
    signal.signal(signal.SIGTERM, lambda signum, _frame: sys.exit(128 + signum))
    if args.captures:
        os.makedirs(args.captures, exist_ok=True)
    sandbox = Sandbox(args.binaries, int(os.environ.get("SANDBOX_PORT", "48090")), extra_env={"FAKES": "1"})
    tui = None
    try:
        phone = Phone(sandbox)
        start_node(sandbox, phone)
        message_the_orchestrator(sandbox)
        deadline = time.time() + TIMEOUT
        while WAITING not in (said := task_says(phone)):
            assert time.time() < deadline, f"the task never read «{WAITING}»: {said[:3000]}"
            time.sleep(0.3)
        queue, _ = phone.queue()
        assert not any(card.startswith("turn-ended-") for card in queue), f"a card asks the user how the task goes on: {queue}"
        print(f"ok (phone): the task reads «{WAITING}» and no card asks how it goes on", file=sys.stderr, flush=True)

        tui = Terminal([sandbox.alondra], sandbox.env, args.captures)
        tui.keys(":")
        tui.type("tasks")
        tui.keys("Enter")
        tui.until(lambda s: "Remove the Fleet" in s, "the task never showed in the tasks screen")
        tui.until(lambda s: "Estado: " + WAITING in s, f"the task's detail never read «{WAITING}»")
        screen = tui.screen()
        assert not any(claim in screen for claim in CLAIMS), f"the tasks screen says the user is needed:\n{screen}"
        tui.capture("tasks")
        tui.keys(":")
        tui.type("fleet")
        tui.keys("Enter")
        tui.until(lambda s: WAITING in s, f"the node in Fleet never read «{WAITING}»")
        tui.capture("fleet")
        queue, _ = phone.queue()
        assert all(card == FAKE_ORCHESTRATOR_CARD or card.startswith(UNANSWERED) for card in queue), f"a card of the task waits for the user: {queue}"
        print(f"ok (tui): the task and its node read «{WAITING}», nothing for the user", file=sys.stderr, flush=True)
        print("e2e turn end wait: a turn that ended on a message to the orchestrator waits for it, with no card for the user.")
    except BaseException:
        if tui:
            print(f"the screen:\n{tui.screen()}", file=sys.stderr, flush=True)
        print("".join(open(os.path.join(sandbox.home, "alondrad.log")).readlines()[-40:]), file=sys.stderr, flush=True)
        raise
    finally:
        if tui:
            tui.stop()
        sandbox.stop()


if __name__ == "__main__":
    main()
