#!/usr/bin/env python3
"""The iOS app in a pooled simulator loses its Mac with the app open and finds it again by itself.

make e2e-ios-offline builds the simulator app with the sandbox's Bonjour service and runs this
against a throwaway sandbox (one Mac, paired over the local network, as on the same Wi-Fi):

1. The Mac's daemon is frozen (SIGSTOP) long enough for every request in flight to time out
   and the next ones to find no route, then thawed. Meanwhile the dot next to the Mac's figures
   and the line under «Waiting for You» must always say the same thing, and at some point that
   the Mac is not answering. A card withdrawn after that must leave the queue on its own, without
   touching the app, and both must show the Mac answering again.
2. With the app in the background the daemon is frozen and thawed again, a second card is
   withdrawn, and the app comes back to the foreground: the queue must catch up at once.

E2E_CAPTURES=<dir> keeps what the app showed at each step. FREEZE=<seconds> changes how long the
Mac stays frozen (90 by default: longer than a held changes poll plus its retry).
"""

import argparse
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import e2e_cards
import e2e_ios_cards

FIRST = "Pick the release channel"
SECOND = "Name the beta build"
ON_ITS_OWN = 45
AT_ONCE = 8


def shown(simulator, need):
    return any(i.startswith("triage.need.") and need[:30] in label for i, label in simulator.elements().items())


def until_gone(simulator, need, seconds, what):
    deadline = time.time() + seconds
    while shown(simulator, need):
        assert time.time() < deadline, what
        time.sleep(0.5)
    return seconds - (deadline - time.time())


# The dot next to the Mac's figures and the line under «Waiting for You» come from one connection
# state: these are the only pairs either may show together.
AGREE = {("live", "summary"), ("connecting", "connecting"), ("down", "savedData")}


def connection_shown(simulator):
    ids = simulator.ids()
    light = next((i.removeprefix("mac.status.") for i in ids if i.startswith("mac.status.")), None)
    line = "savedData" if "connection.savedData" in ids else "connecting" if "connection.connecting" in ids else "summary"
    return light, line


def frozen(daemon, seconds, watch=None):
    """Freezes the daemon for `seconds`, sampling `watch` meanwhile; returns what it saw."""
    seen = []
    os.kill(daemon, signal.SIGSTOP)
    try:
        deadline = time.time() + seconds
        while time.time() < deadline:
            if watch:
                seen.append(watch())
            time.sleep(1)
    finally:
        os.kill(daemon, signal.SIGCONT)
    return seen


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--captures", help="keep what the app showed at each step here")
    parser.add_argument("binaries", nargs="+")
    args = parser.parse_args()
    signal.signal(signal.SIGTERM, lambda signum, _frame: sys.exit(128 + signum))
    ipa = os.environ.get("E2E_IOS_IPA")
    assert ipa and os.path.exists(ipa), "E2E_IOS_IPA names the simulator build of //apple:AlondraiOS with --define=alondra_sandbox=1"
    freeze = int(os.environ.get("FREEZE", "90"))
    if args.captures:
        os.makedirs(args.captures, exist_ok=True)
    os.environ["SANDBOX_LAN"] = "1"
    sandbox = e2e_cards.Sandbox(args.binaries, int(os.environ.get("SANDBOX_PORT", "47960")))
    scratch = tempfile.mkdtemp(prefix="alondra-ios-offline-")
    node = simulator = None
    try:
        node = e2e_cards.Node(sandbox)
        node.call(node.ask("first", FIRST, ["stable", "beta"]), node.ask("second", SECOND, ["one", "two"]))
        simulator = e2e_ios_cards.Simulator(ipa, scratch)
        e2e_ios_cards.pair(simulator, sandbox)
        simulator.until(lambda _: shown(simulator, FIRST), "the first card never showed", 60)
        daemon = int(open(os.path.join(sandbox.home, "alondrad.pid")).read())

        assert connection_shown(simulator) == ("live", "summary"), f"before the freeze: {connection_shown(simulator)}"
        seen = frozen(daemon, freeze, lambda: connection_shown(simulator))
        simulator.capture(args.captures, "after-freeze")
        mixed = [pair for pair in seen if pair not in AGREE]
        assert not mixed, f"the dot and the header disagreed while the Mac was frozen: {mixed}"
        assert ("down", "savedData") in seen, f"a Mac frozen {freeze}s never read as not answering: {sorted(set(seen))}"
        print(f"ok (ios): frozen, the dot and the header agreed in all {len(seen)} samples: {sorted(set(seen))}",
              file=sys.stderr, flush=True)
        node.call(("withdraw", {"interaction_id": "first"}))
        took = until_gone(simulator, FIRST, ON_ITS_OWN, f"the queue never caught up within {ON_ITS_OWN}s of the Mac answering again")
        simulator.capture(args.captures, "caught-up")
        print(f"ok (ios): with the app open, the queue caught up {took:.1f}s after the Mac answered again", file=sys.stderr, flush=True)
        simulator.until(lambda _: connection_shown(simulator) == ("live", "summary"),
                        f"the Mac answers again but the app still shows {connection_shown(simulator)}", AT_ONCE)
        print("ok (ios): the dot is green and the header live again as soon as the Mac answers", file=sys.stderr, flush=True)

        simulator.until(lambda _: shown(simulator, SECOND), "the second card never showed", 30)
        subprocess.run(["xcrun", "simctl", "launch", simulator.udid, "com.apple.Preferences"], capture_output=True)
        frozen(daemon, freeze)
        node.call(("withdraw", {"interaction_id": "second"}))
        subprocess.run(["xcrun", "simctl", "launch", simulator.udid, e2e_ios_cards.BUNDLE], check=True, capture_output=True)
        took = until_gone(simulator, SECOND, AT_ONCE, f"back in the foreground, the queue did not catch up within {AT_ONCE}s")
        simulator.capture(args.captures, "foreground")
        print(f"ok (ios): back in the foreground, the queue caught up in {took:.1f}s", file=sys.stderr, flush=True)
        print("e2e ios offline: the app found its Mac again by itself, and at once on returning to the foreground.")
    finally:
        if simulator:
            simulator.stop()
        if node:
            node.stop()
        sandbox.stop()
        shutil.rmtree(scratch, ignore_errors=True)


if __name__ == "__main__":
    main()
