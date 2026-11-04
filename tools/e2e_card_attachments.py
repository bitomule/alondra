#!/usr/bin/env python3
"""A card with an image, a video and a text file, downloaded and drawn by the clients, against its own sandbox.

A node without a model asks the user with three attachments it stored under the sandbox Mac's
files folder. Two synthetic phones, one over the Mac's direct listener and one over the relay,
read the card's view and download every attachment block by block through the route the view
names, checking the bytes against the card's digest. Then `--client`:

  phone  nothing more: the two downloads are the check.
  ios    the iOS app in a pooled simulator (E2E_IOS_IPA, as make e2e-ios-cards) pairs with the
         sandbox Mac, opens the card, and the captures show the image drawn in the card, the image
         full screen, the video downloading and the video playing, in light and dark.
  keep   leaves the seeded sandbox up until Ctrl+C, printing its port, for the Mac app or a person.

Captures land in --captures; the sandbox and every file it made are removed at the end.
"""

import argparse
import base64
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import e2e_cards  # noqa: E402

NAME = f"Card attachments {os.getpid()}"
NEED = "Revisa la captura y el vídeo de BlurCamera"


def assets(folder):
    """A picture, an eight-second video with sound and a text file, made with ffmpeg."""
    image, video, text = (os.path.join(folder, name) for name in ("captura-blurcamera.png", "demo-blurcamera.mp4", "notas.txt"))
    subprocess.run(["ffmpeg", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "mandelbrot=size=1600x1000:rate=1",
                    "-frames:v", "1", image], check=True)
    subprocess.run(["ffmpeg", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=1280x720:rate=30:duration=8",
                    "-f", "lavfi", "-i", "sine=frequency=440:duration=8", "-c:v", "libx264", "-pix_fmt", "yuv420p",
                    "-c:a", "aac", "-shortest", video], check=True)
    with open(text, "w") as file:
        file.write("Notas de la versión\n\n- Difumina las caras de los vídeos sin subirlos a ningún sitio\n")
    return [image, video, text]


def store(sandbox, paths):
    """Each file copied under the sandbox Mac's files folder, as a node's attachment names it."""
    folder = os.path.join(sandbox.env["ALONDRA_HOME"], "files", "e2e-attachments")
    os.makedirs(folder, exist_ok=True)
    attachments = []
    for path in paths:
        name = os.path.basename(path)
        shutil.copy(path, os.path.join(folder, name))
        data = open(path, "rb").read()
        attachments.append({"id": name, "name": name, "content_digest": hashlib.sha256(data).hexdigest(),
                            "content_reference": f"files/e2e-attachments/{name}", "bytes": len(data)})
    return attachments


def card_attachments(phone):
    deadline = time.time() + e2e_cards.TIMEOUT
    while time.time() < deadline:
        queue, _ = phone.queue()
        for card in queue:
            view = phone.view(f"card:{card}")
            for section in view["sections"]:
                for item in section["items"]:
                    if item.get("need") == NEED and item.get("attachments"):
                        return card, item["attachments"]
        time.sleep(0.5)
    raise AssertionError("the card with attachments never reached the queue")


def download(phone, attachment, route):
    """Every block of one attachment through the route its card names, joined and checked."""
    data, offset, version = b"", 0, None
    while True:
        path = f"{attachment['fetch']}?offset={offset}" + (f"&version={version}" if version else "")
        args = ["request", *(["--direct"] if route == "direct" else []), "GET", path]
        reply = json.loads(phone.run(*args))["response"]
        assert reply["status"] == 200, reply
        block = reply["body"]
        raw = block["data"] + "=" * (-len(block["data"]) % 4)
        data += base64.urlsafe_b64decode(raw)
        offset, version = block["next_offset"], block["version"]
        if block["eof"]:
            break
    assert hashlib.sha256(data).hexdigest() == attachment["digest"], f"{attachment['name']} does not match its digest over {route}"
    return len(data)


class RelayPhone(e2e_cards.Phone):
    """The synthetic phone enrolled at the relay: every request sealed and carried by it."""

    def __init__(self, sandbox):
        self.sandbox = sandbox
        self.env = dict(os.environ, SANDBOX_DIR=sandbox.dir)
        self.run("pair")

    def run(self, *args):
        done = subprocess.run([self.sandbox.tool, "phone", "--name", "e2e-relay", "--mac", "1", *args],
                              env=self.env, capture_output=True, text=True, timeout=120)
        assert done.returncode == 0, done.stderr
        return done.stdout

    def request(self, method, path, body=None):
        reply = json.loads(self.run("request", method, path, *([json.dumps(body)] if body else [])))["response"]
        assert reply["status"] == 200, reply
        return reply["body"]


def check_ios(sandbox, card, attachments, captures):
    import e2e_ios_cards
    import e2e_plan_card
    ipa = os.environ.get("E2E_IOS_IPA")
    assert ipa and os.path.exists(ipa), "E2E_IOS_IPA names the simulator build of //apple:AlondraiOS with --define=alondra_sandbox=1"
    scratch = tempfile.mkdtemp(prefix="alondra-ios-attachments-")
    simulator = e2e_ios_cards.Simulator(ipa, scratch)
    image = next(a["id"] for a in attachments if a["kind"] == "image")
    video = next(a["id"] for a in attachments if a["kind"] == "video")

    def swipe_until(identifier):
        for _ in range(4):
            if identifier in simulator.ids():
                return
            simulator.mav("ui", "swipe", "--start-x", "200", "--start-y", "650", "--end-x", "200", "--end-y", "350")
            time.sleep(e2e_ios_cards.SHOWN)
        assert identifier in simulator.ids(), f"{identifier} never showed"

    try:
        e2e_plan_card.NAME = NAME
        simulator.until(lambda ids: "choosePath.localNetwork" in ids or "findMac.allow" in ids or "findMac.mac" in ids,
                        "the app never offered a way to connect", 60)
        if "choosePath.localNetwork" in simulator.ids():
            simulator.tap("choosePath.localNetwork")
        e2e_plan_card.pair(simulator, sandbox)
        simulator.tap(f"triage.need.{card}")
        simulator.until(lambda ids: "cardDetail.loaded" in ids, "the card's screen stayed loading", e2e_ios_cards.DETAIL)
        swipe_until(f"cardAttachment.image.{image}")
        time.sleep(e2e_ios_cards.SHOWN)
        simulator.capture(captures, "card-image-light")
        subprocess.run(["xcrun", "simctl", "ui", simulator.udid, "appearance", "dark"], check=True)
        time.sleep(e2e_ios_cards.SHOWN)
        simulator.capture(captures, "card-image-dark")
        subprocess.run(["xcrun", "simctl", "ui", simulator.udid, "appearance", "light"], check=True)

        simulator.tap(f"cardAttachment.image.{image}")
        simulator.until(lambda ids: "attachmentViewer.image" in ids, "the image never opened full screen", 30)
        time.sleep(e2e_ios_cards.SHOWN)
        simulator.capture(captures, "image-fullscreen")
        simulator.tap("attachmentViewer.close")
        simulator.until(lambda ids: "attachmentViewer.image" not in ids, "the full-screen image never closed", 30)

        swipe_until(f"cardAttachment.video.{video}")
        simulator.mav("ui", "swipe", "--start-x", "200", "--start-y", "600", "--end-x", "200", "--end-y", "380")
        time.sleep(e2e_ios_cards.SHOWN)
        simulator.capture(captures, "card-video-light")
        subprocess.run(["xcrun", "simctl", "ui", simulator.udid, "appearance", "dark"], check=True)
        time.sleep(e2e_ios_cards.SHOWN)
        simulator.capture(captures, "card-video-dark")
        subprocess.run(["xcrun", "simctl", "ui", simulator.udid, "appearance", "light"], check=True)
        simulator.tap(f"cardAttachment.video.{video}")
        for _ in range(50):
            if f"cardAttachment.downloading.{video}" in simulator.ids():
                simulator.capture(captures, "video-downloading")
                break
            if "attachmentViewer.video" in simulator.ids():
                break
        simulator.until(lambda ids: "attachmentViewer.video" in ids, "the video never played", 120)
        time.sleep(3)
        simulator.capture(captures, "video-playing")
        print("ok (ios): the image is drawn in the card and full screen, and the video downloads and plays", file=sys.stderr, flush=True)
    except BaseException:
        simulator.capture(captures, "failure")
        print(simulator.mav("ui", "tree", check=False), file=sys.stderr, flush=True)
        raise
    finally:
        simulator.stop()
        shutil.rmtree(scratch, ignore_errors=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--client", choices=["phone", "ios", "keep"], required=True)
    parser.add_argument("--captures", required=True)
    parser.add_argument("binaries", nargs="+")
    args = parser.parse_args()
    os.makedirs(args.captures, exist_ok=True)
    os.environ["SANDBOX_NAME"] = NAME
    if args.client == "ios":
        os.environ["SANDBOX_LAN"] = "1"
    sandbox = e2e_cards.Sandbox(args.binaries, int(os.environ.get("SANDBOX_PORT", "47960")))
    node = None
    try:
        made = tempfile.mkdtemp(prefix="assets-", dir=sandbox.scratch)
        attachments = store(sandbox, assets(made))
        node = e2e_cards.Node(sandbox)
        node.call(("ask", {"interaction_id": "attachments", "scope": node.scope(), "recipient": {"kind": "user"}, "title": NEED, "need": NEED,
                           "why": "La versión nueva sale cuando des el visto bueno a lo que se ve en la captura y en el vídeo.",
                           "blocking": False, "attachments": attachments,
                           "options": [{"id": "ok", "label": "Se ve bien", "description": "La versión sale con esta captura y este vídeo"},
                                       {"id": "redo", "label": "Rehacerlos", "description": "Se graban de nuevo antes de publicar"}]}))
        phone = e2e_cards.Phone(sandbox)
        card, served = card_attachments(phone)
        kinds = {a["name"]: a["kind"] for a in served}
        assert kinds == {"captura-blurcamera.png": "image", "demo-blurcamera.mp4": "video", "notas.txt": "text"}, kinds
        for attachment in served:
            size = download(phone, attachment, "direct")
            print(f"ok (direct): {attachment['name']} ({attachment['kind']}, {size} bytes) matches its digest", file=sys.stderr, flush=True)
        relay = RelayPhone(sandbox)
        for attachment in served:
            size = download(relay, attachment, "relay")
            print(f"ok (relay): {attachment['name']} ({attachment['kind']}, {size} bytes) matches its digest", file=sys.stderr, flush=True)
        with open(os.path.join(args.captures, "card-attachments-view.json"), "w") as file:
            json.dump(served, file, ensure_ascii=False, indent=2)
            file.write("\n")
        if args.client == "ios":
            check_ios(sandbox, card, served, args.captures)
        elif args.client == "keep":
            print(f"Sandbox up: daemon on 127.0.0.1:{sandbox.config['macs'][0]['port']}, card {card}. Ctrl+C stops it.", flush=True)
            while True:
                try:
                    sandbox.approve_pairing(seconds=3600)
                except AssertionError:
                    pass
    except KeyboardInterrupt:
        pass
    finally:
        if node:
            node.stop()
        sandbox.stop()


if __name__ == "__main__":
    main()
