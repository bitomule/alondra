#!/usr/bin/env python3
"""The real Live Activity in a pooled simulator: captures of it on the Lock Screen.

make e2e-ios-live-activity builds the simulator app with the sandbox's Bonjour service, pairs it with
a throwaway sandbox that has cards waiting, and captures the Lock Screen, light and dark, in English
and Spanish. The app writes the counts itself here: `simctl push` takes no Live Activity pushes, so
what a push does needs a real iPhone. So does the Dynamic Island: the simulator widens it for the
activity, but `simctl io screenshot` leaves what it shows out of the image.

E2E_CAPTURES=<dir> says where the PNGs go.
"""

import argparse
import os
import shutil
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import e2e_cards
import e2e_ios_cards

SETTLE = 3
ALLOW = (286, 701)
LANGUAGES = {"en": "en_US", "es": "es_ES"}


def simctl(*args):
    subprocess.run(["xcrun", "simctl", *args], check=True, capture_output=True)


def axe(simulator, *args):
    subprocess.run(["axe", *args, "--udid", simulator.udid], check=True, capture_output=True)


def speak(simulator, language):
    """A widget follows the system's language, not the app's, so the simulator starts again in it."""
    simctl("spawn", simulator.udid, "defaults", "write", ".GlobalPreferences", "AppleLanguages", "-array", language)
    simctl("spawn", simulator.udid, "defaults", "write", ".GlobalPreferences", "AppleLocale", "-string", LANGUAGES[language])
    simctl("shutdown", simulator.udid)
    simctl("boot", simulator.udid)
    simctl("bootstatus", simulator.udid)


def open_app(simulator):
    env = dict(os.environ, SIMCTL_CHILD_ALONDRA_SANDBOX="1")
    subprocess.run(["xcrun", "simctl", "launch", "--terminate-running-process", simulator.udid, e2e_ios_cards.BUNDLE],
                   env=env, check=True, capture_output=True)
    simulator.until(lambda ids: any(i.startswith("triage.need.") for i in ids), "the queue never showed", 60)
    time.sleep(SETTLE)


def lock(simulator):
    axe(simulator, "button", "home")
    axe(simulator, "button", "lock")
    time.sleep(1)
    axe(simulator, "button", "lock")
    time.sleep(SETTLE)


def allow(simulator):
    """iOS asks on the Lock Screen beside the Live Activity, once at first and again after a restart."""
    lock(simulator)
    axe(simulator, "tap", "-x", str(ALLOW[0]), "-y", str(ALLOW[1]))
    time.sleep(SETTLE)


def capture(simulator, captures, name):
    time.sleep(SETTLE)
    simctl("io", simulator.udid, "screenshot", os.path.join(captures, f"{name}.png"))
    print(f"ok: {name}", file=sys.stderr, flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--captures", required=True)
    parser.add_argument("binaries", nargs="+")
    args = parser.parse_args()
    ipa = os.environ.get("E2E_IOS_IPA")
    assert ipa and os.path.exists(ipa), "E2E_IOS_IPA names the simulator build of //apple:AlondraiOS with --define=alondra_sandbox=1"
    os.makedirs(args.captures, exist_ok=True)
    os.environ["SANDBOX_LAN"] = "1"
    # A slim simulator runs without the daemons that show a Live Activity.
    os.environ["SIMPOOL_NEED"] = "widgets"
    sandbox = e2e_cards.Sandbox(args.binaries, int(os.environ.get("SANDBOX_PORT", "47970")))
    scratch = tempfile.mkdtemp(prefix="alondra-ios-live-activity-")
    node = simulator = None
    try:
        node = e2e_cards.Node(sandbox)
        node.call(node.ask("first", "Pick the release channel", ["stable", "beta"]), node.ask("second", "Name the beta build", ["one", "two"]))
        simulator = e2e_ios_cards.Simulator(ipa, scratch)
        e2e_ios_cards.pair(simulator, sandbox)
        for language in LANGUAGES:
            speak(simulator, language)
            open_app(simulator)
            allow(simulator)
            for appearance in ("light", "dark"):
                simctl("ui", simulator.udid, "appearance", appearance)
                open_app(simulator)
                lock(simulator)
                capture(simulator, args.captures, f"lock-screen-{appearance}-{language}")
        print(f"e2e ios live activity: captures in {args.captures}")
    finally:
        if simulator:
            simulator.stop()
        if node:
            node.stop()
        sandbox.stop()
        shutil.rmtree(scratch, ignore_errors=True)


if __name__ == "__main__":
    main()
