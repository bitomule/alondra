#!/usr/bin/env python3
"""A task whose background command ended without waking Claude offers Interrupt and Pause, in the
real TUI in tmux, and Interrupt hands Claude the message it held.

The task's node is the sandbox's scripted native Claude in the Mac's Agent SDK host. Its turn ends
with a command left in the background; the command ends and Claude never takes up its
notification, as on 22 October with «Parcel: widget de issues abiertos». The task's screen must offer
Interrumpir and Pausar; a message sent then stays queued, and pressing Interrumpir lets Claude take
it. No model, no account, no tokens; everything is removed at the end.

  tools/e2e_interrupt_unheard.py [--captures DIR] <alondrad> <alondra> <alondra-relay> <alondra-sandbox>
  (SDK_NODE and SDK_HOST name the pinned Node and the host bundle)
"""

import argparse
import json
import os
import signal
import subprocess
import sys
import time
import uuid

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from e2e_cards import TIMEOUT, Phone, Sandbox, Terminal  # noqa: E402
from e2e_turn_end_wait import session  # noqa: E402

TASK, REQUEST, TITLE = "t-phases", "req-phases", "Snapshots in the background"
MESSAGE = "Relaunch the snapshots and add the Spanish captures"


def start_node(sandbox, phone):
    scripts = os.path.join(sandbox.dir, "fakes", "scripts")
    os.makedirs(scripts, exist_ok=True)
    with open(os.path.join(scripts, f"{REQUEST}.json"), "w") as script:
        json.dump({"request_id": REQUEST, "tasks": [{"task_id": TASK, "title": TITLE}]}, script)
    phone.request("POST", "/requests", {"request_id": REQUEST, "text": TITLE})
    deadline = time.time() + TIMEOUT
    while not session(sandbox):
        assert time.time() < deadline, f"Alondra started no node for {TASK}"
        time.sleep(0.2)


def step(sandbox, *steps):
    run = subprocess.run([sandbox.tool, "--sandbox", sandbox.dir, "fake", "node", TASK, "call", "--step", json.dumps(list(steps))],
                         env=dict(os.environ, SANDBOX_DIR=sandbox.dir), capture_output=True, text=True, timeout=TIMEOUT * 2)
    assert run.returncode == 0 and run.stdout.startswith("ok "), f"the node could not take {steps}: {run.stdout}\n{run.stderr}"


def actions(phone):
    view = phone.view(f"task:{TASK}")
    found = [a for section in view["sections"] for item in section["items"] for a in (item.get("actions") or []) + (item.get("more") or [])]
    return found, view["data_epoch"]


def offered(phone, prefix):
    return next((action for action in actions(phone)[0] if action["id"].startswith(prefix)), None)


def press(phone, prefix, text=None):
    found, epoch = actions(phone)
    action = next(a for a in found if a["id"].startswith(prefix))
    return phone.request("POST", "/actions", {"protocol": 1, "command_id": str(uuid.uuid4()), "data_epoch": epoch, "client": "ios",
                                              "lang": "es", "action_id": action["id"], "input": {"text": text} if text else {}})


def received(sandbox):
    found = session(sandbox)
    path = os.path.join(sandbox.dir, "fakes", "sessions", found["session_id"][:8], "received.jsonl") if found else ""
    return open(path).read() if os.path.exists(path) else ""


def took_the_message(phone):
    """The sandbox's Claude answers what it takes with «Done.», which the task shows as what it said last."""
    return '"Done."' in json.dumps(phone.view(f"task:{TASK}"), ensure_ascii=False)


def wait(what, predicate):
    deadline = time.time() + TIMEOUT
    while not predicate():
        assert time.time() < deadline, what
        time.sleep(0.3)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--captures", help="keep what the TUI showed at each step here")
    parser.add_argument("binaries", nargs="+")
    args = parser.parse_args()
    signal.signal(signal.SIGTERM, lambda signum, _frame: sys.exit(128 + signum))
    if args.captures:
        os.makedirs(args.captures, exist_ok=True)
    sandbox = Sandbox(args.binaries, int(os.environ.get("SANDBOX_PORT", "48130")), extra_env={"FAKES": "1"})
    tui = None
    try:
        phone = Phone(sandbox)
        start_node(sandbox, phone)
        step(sandbox, {"do": "speak", "said": "Running the snapshots in the background", "behind": "sleep 600"})
        step(sandbox, {"do": "background_ends_unheard"})
        wait("the task never offered Interrupt and Pause while its background command ended unheard",
             lambda: offered(phone, "interrupt:") and offered(phone, "pause:"))
        print("ok (phone): the task offers Interrumpir and Pausar", file=sys.stderr, flush=True)

        tui = Terminal([sandbox.alondra], sandbox.env, args.captures)
        tui.keys(":")
        tui.type("tasks")
        tui.keys("Enter")
        tui.until(lambda s: TITLE in s, "the task never showed in the tasks screen")
        tui.until(lambda s: "Interrumpir" in s and "Pausar" in s, "the task's detail never offered Interrumpir and Pausar")
        tui.capture("waiting-unheard")
        print("ok (tui): the task offers Interrumpir and Pausar", file=sys.stderr, flush=True)

        sent = press(phone, "msg:", MESSAGE)
        print(f"sent: {sent.get('stamp')}", file=sys.stderr, flush=True)
        wait("Claude never got the message", lambda: MESSAGE in received(sandbox))
        time.sleep(2)
        assert offered(phone, "interrupt:") and not took_the_message(phone), "Claude took the message before it was interrupted"
        tui.capture("message-held")
        assert press(phone, "interrupt:")["outcome"] == "applied"
        wait("the message never ran once the task was interrupted", lambda: took_the_message(phone))
        tui.until(lambda s: "Done." in s, "the TUI never showed what Claude answered to the message")
        tui.capture("interrupted")
        print("ok: Interrumpir let Claude take the message it held", file=sys.stderr, flush=True)
        print("e2e interrupt unheard: a task whose background work ended unheard offers Interrupt and Pause, and Interrupt delivers what waited.")
    except BaseException:
        if tui:
            print(f"the screen:\n{tui.screen()}", file=sys.stderr, flush=True)
        print("".join(open(os.path.join(sandbox.home, "alondrad.log")).readlines()[-40:]), file=sys.stderr, flush=True)
        found = session(sandbox)
        if found:
            native = os.path.join(sandbox.dir, "fakes", "sessions", found["session_id"][:8], "native.log")
            print(open(native).read()[-3000:] if os.path.exists(native) else "no native log", file=sys.stderr, flush=True)
        raise
    finally:
        if tui:
            tui.stop()
        sandbox.stop()


if __name__ == "__main__":
    main()
