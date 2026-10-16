#!/usr/bin/env python3
"""Cancelling a working task in a throwaway sandbox, seen in the real TUI in tmux.

A node (the probe) works on its task. The user cancels it from the phone; the TUI reads
«Cancelando» while Alondra records the end of the node, and «Cancelada» once that receipt is in.
Nothing before the receipt makes the task cancelled.

  tools/e2e_cancel.py [--captures DIR] <alondrad> <alondra> <alondra-relay> <alondra-sandbox>
"""

import argparse
import os
import signal
import sys
import time
import uuid

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from e2e_cards import TIMEOUT, Node, Phone, Sandbox, Terminal  # noqa: E402

CANCELLING = "Cancelando"
CANCELLED = "Cancelada"


def tasks_screen(tui):
    tui.keys(":")
    tui.type("tasks")
    tui.keys("Enter")


def facts(value, out):
    """Every object in `value` with a label and a value, depth first: the task's facts."""
    if isinstance(value, dict):
        if value.get("label") and value.get("value"):
            out.append(value)
        for inner in value.values():
            facts(inner, out)
    elif isinstance(value, list):
        for inner in value:
            facts(inner, out)


def task_state(phone, task):
    found = []
    facts(phone.view(f"task:{task}"), found)
    state = next((fact["value"] for fact in found if fact["label"] in ("State", "Estado")), "")
    return state.split(" · ")[0]


def cancel(phone, task):
    view = phone.view(f"task:{task}")
    actions = [a for section in view["sections"] for item in section["items"] for a in (item.get("actions") or []) + (item.get("more") or [])]
    action = next(a for a in actions if a["id"].startswith("cancel:"))
    reply = phone.request("POST", "/actions", {"protocol": 1, "command_id": str(uuid.uuid4()), "data_epoch": view["data_epoch"],
                                               "client": "ios", "lang": "es", "action_id": action["id"]})
    assert reply["outcome"] == "applied", reply


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--captures", help="keep what the TUI showed at each step here")
    parser.add_argument("binaries", nargs="+")
    args = parser.parse_args()
    signal.signal(signal.SIGTERM, lambda signum, _frame: sys.exit(128 + signum))
    if args.captures:
        os.makedirs(args.captures, exist_ok=True)
    sandbox = Sandbox(args.binaries, int(os.environ.get("SANDBOX_PORT", "47970")))
    node, tui = None, None
    try:
        node = Node(sandbox)
        tui = Terminal([sandbox.alondra], sandbox.env, args.captures)
        tasks_screen(tui)
        tui.until(lambda s: "Probe" in s, "the probe's task never showed in the tasks screen")
        tui.capture("working")
        phone = Phone(sandbox, direct=True)
        cancel(phone, node.task)
        tui.until(lambda s: "Estado: " + CANCELLING in s, "the task never read «Cancelando» after the user cancelled it")
        tui.capture("cancelling")
        print("ok (tui): the cancelled task reads «Cancelando» until the end of its node is recorded", file=sys.stderr, flush=True)
        tui.until(lambda s: "TERMINADAS 1" in s and CANCELLING not in s, "the task never left the tasks in progress", seconds=TIMEOUT * 2)
        tui.keys("Enter")
        tui.until(lambda s: "▾ TERMINADAS" in s, "the finished section never opened")
        time.sleep(3)
        tui.capture("finished")
        state = task_state(phone, node.task)
        assert state.startswith(CANCELLED), f"the task reads {state!r} once its node's end is recorded"
        print(f"ok (tui): the task left the tasks in progress for the finished ones, and reads «{state}»", file=sys.stderr, flush=True)
        print("e2e cancel: the task is cancelling while its node is closed, and cancelled only with the receipt.")
    except BaseException:
        if tui:
            print(f"the screen:\n{tui.screen()}", file=sys.stderr, flush=True)
        print("".join(open(os.path.join(sandbox.home, "alondrad.log")).readlines()[-40:]), file=sys.stderr, flush=True)
        raise
    finally:
        if tui:
            tui.stop()
        if node:
            node.stop()
        sandbox.stop()


if __name__ == "__main__":
    main()
