#!/usr/bin/env python3
"""Pausing a task that waits for its automatic start takes it out of the queue, in a throwaway
sandbox with the real TUI in tmux.

16 October: with 15 tasks queued, Pause answered «Esta tarea no está en cola» for every task
waiting to start by itself. Here the working-node limit is 1 and a node holds it; the orchestrator
(tools/e2e_pause_queued_orchestrator.py) creates three tasks with launch plans, which queue. In the
TUI the user opens one, presses Pause and sees it «En pausa», out of the queue.

  tools/e2e_pause_queued.py [--captures DIR] <alondrad> <alondra> <alondra-relay> <alondra-sandbox>
"""

import argparse
import os
import signal
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from e2e_cards import TIMEOUT, Node, Sandbox, Terminal  # noqa: E402

TITLE = "Redraft index UX"
QUEUED = "En cola"
PAUSED = "En pausa"


def wait_for(path, what, seconds=TIMEOUT):
    deadline = time.time() + seconds
    while not os.path.exists(path):
        assert time.time() < deadline, what
        time.sleep(0.5)


def line_of(screen, text):
    return next((line for line in screen.splitlines() if text in line), "")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--captures", help="keep what the TUI showed at each step here")
    parser.add_argument("binaries", nargs="+")
    args = parser.parse_args()
    signal.signal(signal.SIGTERM, lambda signum, _frame: sys.exit(128 + signum))
    if args.captures:
        os.makedirs(args.captures, exist_ok=True)
    sandbox = Sandbox(args.binaries, int(os.environ.get("SANDBOX_PORT", "47970")), orchestrator="tools/e2e_pause_queued_orchestrator.py",
                      extra_env={"E2E_PROJECT": "/private/tmp"})
    node, tui = None, None
    try:
        with open(os.path.join(sandbox.home, "options.toml"), "a") as options:
            options.write("\nmax_working_nodes = 1\n")
        node = Node(sandbox)
        node.call(("ask", {"interaction_id": "queue-tasks", "scope": node.scope(), "recipient": {"kind": "orchestrator"},
                           "need": "Queue three tasks for this request", "why": "They wait for a free slot.", "blocking": False,
                           "content": {"kind": "report", "body": f"Queue three tasks after {node.task}"},
                           "options": [{"id": "done", "label": "Done", "description": "They exist"},
                                       {"id": "no", "label": "No", "description": "Not now"}]}))
        wait_for(sandbox.orchestrator_log + ".created", "the orchestrator never created the tasks", seconds=TIMEOUT * 3)
        tui = Terminal([sandbox.alondra], sandbox.env, args.captures)
        tui.keys(":")
        tui.type("tasks")
        tui.keys("Enter")
        tui.until(lambda s: QUEUED in line_of(s, TITLE), "the task never read as queued")
        tui.keys("j")
        tui.keys("j")
        tui.until(lambda s: "││" + TITLE in s and "Pausar" in s, "the queued task offers no Pause")
        tui.capture("queued")
        tui.keys("p")
        tui.until(lambda s: PAUSED in line_of(s, TITLE) or "Reanudar" in s, "the task never read as paused")
        screen = tui.screen()
        tui.capture("paused")
        assert "no está en cola" not in screen, screen
        print("ok (tui): Pause took the queued task out of the queue", file=sys.stderr, flush=True)
        if os.environ.get("E2E_HOLD"):
            print(f"holding: tmux -L {tui.socket} attach", file=sys.stderr, flush=True)
            while not os.path.exists(os.environ["E2E_HOLD"]):
                time.sleep(1)
        print("e2e pause queued: a task waiting for its automatic start was paused from the TUI and left the queue.")
    except BaseException:
        if tui:
            print(f"the screen:\n{tui.screen()}", file=sys.stderr, flush=True)
        print("".join(open(os.path.join(sandbox.home, "alondrad.log")).readlines()[-40:]), file=sys.stderr, flush=True)
        for log in (sandbox.orchestrator_log, sandbox.orchestrator_log + ".errors"):
            if os.path.exists(log):
                print(open(log).read()[-4000:], file=sys.stderr, flush=True)
        if os.environ.get("E2E_HOLD"):
            print(f"holding after failure: {sandbox.scratch}", file=sys.stderr, flush=True)
            while not os.path.exists(os.environ["E2E_HOLD"]):
                time.sleep(1)
        raise
    finally:
        if tui:
            tui.stop()
        if node:
            node.stop()
        sandbox.stop()


if __name__ == "__main__":
    main()
