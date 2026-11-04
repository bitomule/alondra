#!/usr/bin/env python3
"""A decision of the user holds the tasks after it until the orchestrator reviews their plans, in a
throwaway sandbox with the real TUI in tmux.

15 October: on Redraft's phase B the user chose 512 px prints, and phases C and D waited with plans
for the 360 px copy. Here a node (phase B) asks for its phases, then asks the user that decision;
the user answers in the TUI. C and D read «Revisión de plan pendiente». The orchestrator
(tools/e2e_plan_review_orchestrator.py) leaves them unreviewed until the user, on the problem
card, reminds it; then it gives C a new plan and keeps D's, and neither is held any more.

  tools/e2e_plan_review.py [--captures DIR] <alondrad> <alondra> <alondra-relay> <alondra-sandbox>
"""

import argparse
import os
import signal
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from e2e_cards import TIMEOUT, Node, Sandbox, Terminal  # noqa: E402

HELD = "Revisión de plan pendiente"
DECISION = "Decide the index's next step"
PROBLEM = "esperan revisión de plan"


def wait_for(path, what, seconds=TIMEOUT):
    deadline = time.time() + seconds
    while not os.path.exists(path):
        assert time.time() < deadline, what
        time.sleep(0.5)


def tasks_screen(tui):
    tui.keys(":")
    tui.type("tasks")
    tui.keys("Enter")


def inbox_screen(tui):
    tui.keys(":")
    tui.type("inbox")
    tui.keys("Enter")


def phases_held(home):
    held = subprocess.run(["sqlite3", "-readonly", os.path.join(home, "domain.db"), "select review from plan_reviews"],
                          capture_output=True, text=True, check=True).stdout
    return held.count('"phase-c":null') + held.count('"phase-d":null')


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--captures", help="keep what the TUI showed at each step here")
    parser.add_argument("binaries", nargs="+")
    args = parser.parse_args()
    signal.signal(signal.SIGTERM, lambda signum, _frame: sys.exit(128 + signum))
    if args.captures:
        os.makedirs(args.captures, exist_ok=True)
    sandbox = Sandbox(args.binaries, int(os.environ.get("SANDBOX_PORT", "47960")), orchestrator="tools/e2e_plan_review_orchestrator.py",
                      extra_env={"E2E_PROJECT": "/private/tmp"})
    node, tui = None, None
    try:
        node = Node(sandbox)
        node.call(("ask", {"interaction_id": "create-phases", "scope": node.scope(), "recipient": {"kind": "orchestrator"},
                           "title": "Create the phases that come after my task", "need": "Create the phases that come after my task", "why": "The plan has two more phases.", "blocking": False,
                           "content": {"kind": "report", "body": f"Create the phases after {node.task}"},
                           "options": [{"id": "done", "label": "Done", "description": "They exist"},
                                       {"id": "no", "label": "No", "description": "Not now"}]}))
        wait_for(sandbox.orchestrator_log + ".created", "the orchestrator never created the phases", seconds=TIMEOUT * 3)
        node.call(("ask", {"interaction_id": "decision", "scope": node.scope(), "recipient": {"kind": "user"}, "title": DECISION, "need": DECISION,
                           "why": "Phases C and D start after it.", "blocking": True,
                           "content": {"kind": "report", "body": "With the 360 px copy only 11 of 21 groups match."},
                           "options": [{"id": "threshold", "label": "Adjust the threshold", "description": "Measure again with +0.10"},
                                       {"id": "plan-512", "label": "512 px prints", "description": "Download 512 px images at night"}]}))
        tui = Terminal([sandbox.alondra], sandbox.env, args.captures)
        tui.until(lambda s: DECISION[:30] in s, "the decision never showed in the inbox")
        tui.capture("decision")
        tui.keys("2")
        tui.until(lambda s: DECISION[:30] not in s, "the decision stayed in the inbox once answered")
        tasks_screen(tui)
        tui.until(lambda s: s.count(HELD) >= 2, "phases C and D never read as held")
        tui.capture("held")
        print("ok (tui): both phases read «Revisión de plan pendiente» after the decision", file=sys.stderr, flush=True)
        assert phases_held(sandbox.home) == 2
        inbox_screen(tui)
        tui.until(lambda s: PROBLEM in s, "the unreviewed plans never became the user's problem", seconds=TIMEOUT * 3)
        tui.capture("problem")
        print("ok (tui): the plans left unreviewed reach the user as a problem", file=sys.stderr, flush=True)
        open(sandbox.orchestrator_log + ".settle", "w").close()
        tui.keys("1")
        wait_for(sandbox.orchestrator_log + ".settled", "reminding the orchestrator never got the plans reviewed", seconds=TIMEOUT * 2)
        tasks_screen(tui)
        tui.until(lambda s: s.count("Espera a «") >= 2 and HELD not in s,
                  "the phases did not go back to waiting for phase B once reviewed")
        tui.capture("reviewed")
        assert phases_held(sandbox.home) == 0
        mission = subprocess.run(["sqlite3", "-readonly", os.path.join(sandbox.home, "domain.db"),
                                  "select json_extract(plan,'$.mission') from task_launch_plans where task_id='phase-c'"],
                                 capture_output=True, text=True, check=True).stdout.strip()
        assert mission == "Group with 512 px prints", mission
        print("e2e plan review: the decision held both phases, the user heard of it, and they were released only by their review.")
    except BaseException:
        if tui:
            print(f"the screen:\n{tui.screen()}", file=sys.stderr, flush=True)
        print("".join(open(os.path.join(sandbox.home, "alondrad.log")).readlines()[-40:]), file=sys.stderr, flush=True)
        print(sorted(os.listdir(sandbox.scratch)), file=sys.stderr, flush=True)
        for log in (sandbox.orchestrator_log, sandbox.orchestrator_log + ".errors"):
            if os.path.exists(log):
                print(open(log).read()[-6000:], file=sys.stderr, flush=True)
        raise
    finally:
        if tui:
            tui.stop()
        if node:
            node.stop()
        sandbox.stop()


if __name__ == "__main__":
    main()
