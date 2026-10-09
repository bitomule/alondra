"""The iOS app in a pooled simulator, paired with the e2e sandbox over its direct listener.

Used by `tools/e2e_cards.py --client ios` (make e2e-ios-cards). The app must be the simulator build
with the sandbox's Bonjour service (`--define=alondra_sandbox=1`), given as E2E_IOS_IPA. MAV drives
it: the front card of "Waiting for You" is answered as a person would, each card of a group is
opened on its own screen first, and every answered card must be gone from the screen at once and
stay gone. The simulator comes from simpool and goes back to it at the end.
"""

import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import zipfile

BUNDLE = "com.bitomule.alondra"
SHOWN = 1.5
DETAIL = 15


class Simulator:
    def __init__(self, ipa, scratch):
        self.key = f"alondra-e2e-cards-{os.getpid()}"
        self.udid = subprocess.run(["simpool", "lease", "--device", "iPhone 17 Pro", "--os", "26.3", "--key", self.key, "--ttl", "30m"],
                                   check=True, capture_output=True, text=True).stdout.strip()
        app = os.path.join(scratch, "app")
        zipfile.ZipFile(ipa).extractall(app)
        bundle = os.path.join(app, "Payload", next(name for name in os.listdir(os.path.join(app, "Payload")) if name.endswith(".app")))
        subprocess.run(["xcrun", "simctl", "uninstall", self.udid, BUNDLE], capture_output=True)
        subprocess.run(["xcrun", "simctl", "keychain", self.udid, "reset"], check=True, capture_output=True)
        subprocess.run(["xcrun", "simctl", "install", self.udid, bundle], check=True)
        self.root = os.path.join(scratch, "mav")
        os.makedirs(os.path.join(self.root, ".mav"))
        with open(os.path.join(self.root, ".mav", "config.yaml"), "w") as config:
            config.write(f"target_kind: simulator\napp:\n  bundle_id: {BUNDLE}\n  process_name: AlondraiOS\nlaunch:\n  mode: already_installed\n")
        env = dict(os.environ, SIMCTL_CHILD_ALONDRA_SANDBOX="1")
        subprocess.run(["xcrun", "simctl", "launch", "--terminate-running-process", self.udid, BUNDLE,
                        "-AppleLanguages", "(es)", "-AppleLocale", "es_ES"], env=env, check=True, capture_output=True)

    def mav(self, *args, check=True):
        env = dict(os.environ, MAV_TARGET_KIND="simulator", MAV_TARGET_UDID=self.udid)
        done = subprocess.run(["mav", *args], cwd=self.root, env=env, capture_output=True, text=True)
        for _ in range(2):
            if done.returncode == 0 or args[:2] != ("ui", "tree"):
                break
            time.sleep(1)
            done = subprocess.run(["mav", *args], cwd=self.root, env=env, capture_output=True, text=True)
        if check and done.returncode != 0:
            raise AssertionError(f"mav {' '.join(args)}: {done.stdout}{done.stderr}")
        return done.stdout

    def elements(self):
        """Each element with an identifier, and its label."""
        found = {}
        for line in self.mav("ui", "tree").splitlines():
            match = re.search(r' id=(\S+)(?: label=("[^"]*"|\S+))?', line)
            if match:
                found[match.group(1)] = (match.group(2) or "").strip('"')
        return found

    def ids(self):
        return set(self.elements())

    def until(self, predicate, what, seconds):
        deadline = time.time() + seconds
        while not predicate(ids := self.ids()):
            if time.time() > deadline:
                raise AssertionError(what)
            time.sleep(0.2)
        return ids

    def tap(self, identifier):
        self.mav("ui", "tap", "--id", identifier)

    def tap_trailing_edge(self, identifier):
        """The reply's send button reports the whole reply row as its frame: its centre is the
        text field, so the tap goes where the button is drawn, at the row's trailing end."""
        line = next(line for line in self.mav("ui", "tree").splitlines() if f" id={identifier} " in line)
        x, y, width, height = map(float, re.findall(r"[-\d.]+", line.split('frame="')[1].split('"')[0]))
        self.mav("ui", "tap", "--x", str(int(x + width - 24)), "--y", str(int(y + height / 2)))

    def capture(self, captures, name):
        if captures:
            path = self.mav("capture", "--name", name).split("file=")[1].split(" ")[0]
            shutil.copy(path, os.path.join(captures, f"ios-{name}.png"))

    def stop(self):
        subprocess.run(["xcrun", "simctl", "terminate", self.udid, BUNDLE], capture_output=True)
        subprocess.run(["xcrun", "simctl", "uninstall", self.udid, BUNDLE], capture_output=True)
        subprocess.run(["xcrun", "simctl", "keychain", self.udid, "reset"], capture_output=True)
        subprocess.run(["simpool", "release", "--key", self.key], capture_output=True)


def check(sandbox, answers, kind_of, captures):
    ipa = os.environ.get("E2E_IOS_IPA")
    assert ipa and os.path.exists(ipa), "E2E_IOS_IPA names the simulator build of //apple:AlondraiOS with --define=alondra_sandbox=1"
    scratch = tempfile.mkdtemp(prefix="alondra-ios-cards-")
    simulator = None
    try:
        simulator = Simulator(ipa, scratch)
        simulator.until(lambda ids: "findMac.allow" in ids or "findMac.mac" in ids, "the app never showed pairing", 60)
        if "findMac.allow" in simulator.ids():
            simulator.tap("findMac.allow")
        simulator.until(lambda ids: "findMac.mac" in ids, "the sandbox Mac never appeared on Bonjour", 60)
        simulator.tap("findMac.mac")
        sandbox.approve_pairing()
        simulator.until(lambda ids: any(i.startswith("triage.need.") for i in ids), "the queue never showed", 60)
        simulator.capture(captures, "queue")
        answered = []
        skipped = 0
        while True:
            elements = simulator.elements()
            shown = {i.removeprefix("triage.need."): label for i, label in elements.items() if i.startswith("triage.need.")}
            if not any(kind_of(label) for label in shown.values()):
                break
            card = next(iter(shown)) if len(shown) == 1 else front_of(simulator, list(shown))
            kind = kind_of(shown[card])
            if kind is None:
                skipped += 1
                assert skipped < 5, f"only cards the sandbox raised by itself are in front: {shown}"
                simulator.tap(f"triage.later.{card}")
                time.sleep(1)
                continue
            if kind.startswith("group-q"):
                simulator.tap(f"triage.need.{card}")
                simulator.until(lambda ids: "cardDetail.loaded" in ids, f"the screen of {card} stayed loading", DETAIL)
                simulator.capture(captures, f"detail-{card}")
                print(f"ok (ios): the screen of {card} loads", file=sys.stderr, flush=True)
                simulator.mav("ui", "tap", "--x", "33", "--y", "75")
                simulator.until(lambda ids: f"triage.need.{card}" in ids, "the queue never came back", DETAIL)
            prefix, option, text = answers[kind]
            if prefix == "reply:":
                simulator.tap(f"triage.reply.{card}")
                simulator.mav("ui", "type", text)
                simulator.tap_trailing_edge(f"triage.send.{card}")
            else:
                button = next(i for i in simulator.ids() if i.startswith(f"triage.action.{card}.")
                              and (option is None or i.endswith(f".{option}")))
                simulator.tap(button)
            simulator.until(lambda ids: f"triage.need.{card}" not in ids, f"{card} was still on screen right after it was answered", SHOWN)
            simulator.capture(captures, f"answered-{card}")
            answered.append(card)
            print(f"ok (ios): {card} left at once", file=sys.stderr, flush=True)
        time.sleep(20)
        ids = simulator.ids()
        back = [card for card in answered if f"triage.need.{card}" in ids]
        assert not back, f"answered cards came back: {back}"
        simulator.capture(captures, "empty")
        print("ok (ios): no answered card came back", file=sys.stderr, flush=True)
    finally:
        if simulator:
            simulator.stop()
        shutil.rmtree(scratch, ignore_errors=True)


def front_of(simulator, shown):
    """Of the cards drawn in the stack, the front one sits lowest: the ones behind peek out above it."""
    positions = {}
    for line in simulator.mav("ui", "tree").splitlines():
        for card in shown:
            if f"id=triage.later.{card} " in line and "frame=" in line:
                y = float(line.split("frame=\"{{")[1].split(",")[1].split("}")[0])
                positions[card] = y
    return max(positions, key=positions.get)
