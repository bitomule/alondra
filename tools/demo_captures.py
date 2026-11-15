#!/usr/bin/env python3
"""Real screenshots of the TUI, the iPhone app and the Mac app, from a sandbox seeded with the demo
world (invented projects, no data of anyone's own): `alondra-sandbox journey demo`.

    tools/demo_captures.py --out DIR [--only tui,ios,mac] [--ios-ipa IPA] [--mac-zip ZIP] BINARIES...

BINARIES are alondrad, alondra, alondra-relay and alondra-sandbox, as tools/e2e-sandbox.sh takes
them; SDK_NODE and SDK_HOST name the pinned Node and the host bundle. Every client is shot in
English and Spanish, light and dark. The TUI is drawn by the terminal, so its light and dark
shots only change the terminal's own background. DIR is emptied first; the sandbox, the tmux server,
the simulator lease and the app copies are all removed when the run ends, however it ends.
"""

import argparse
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import unicodedata
import time
import urllib.request
import zipfile

from PIL import Image, ImageDraw, ImageFont

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LANGUAGES = {"en": "en_US", "es": "es_ES"}
APPEARANCES = ("light", "dark")
TIMEOUT = 90
BUNDLE = "com.bitomule.alondra"
REVIEW_MARK = "PR #12"
DEMO_PROJECTS = "/Users/Shared/Projects"
DEMO_PROJECT = "lumen"

PALETTES = {
    "light": {"background": (250, 250, 250), "foreground": (30, 30, 30)},
    "dark": {"background": (24, 24, 27), "foreground": (228, 228, 231)},
}
BASIC_COLORS = [(0, 0, 0), (205, 49, 49), (13, 188, 121), (229, 229, 16), (36, 114, 200), (188, 63, 188), (17, 168, 205), (229, 229, 229),
                (102, 102, 102), (241, 76, 76), (35, 209, 139), (245, 245, 67), (59, 142, 234), (214, 112, 214), (41, 184, 219), (255, 255, 255)]


class Sandbox:
    def __init__(self, binaries, directory, port):
        self.dir = directory
        self.env = dict(os.environ, SANDBOX_DIR=self.dir, SANDBOX_PORT=str(port), MACS="1", FAKES="1", SANDBOX_LAN="1")
        for name in [name for name in self.env if name.startswith("ALONDRA_")]:
            del self.env[name]
        subprocess.run([os.path.join(REPO, "tools/e2e-sandbox.sh"), *binaries], env=self.env, check=True, stdout=subprocess.DEVNULL)
        subprocess.run([os.path.join(self.dir, "bin/alondra-sandbox"), "--sandbox", self.dir, "journey", "demo"], env=self.env, check=True, stdout=subprocess.DEVNULL)
        self.home = os.path.realpath(os.path.join(self.dir, "mac-1"))
        with open(os.path.join(self.home, "env")) as env:
            self.port = re.search(r'^export ALONDRA_PORT="(.*)"$', env.read(), re.M).group(1)

    def approve_pairing(self):
        """A person on the Mac taps Allow on the phone's pairing request."""
        local = f"http://127.0.0.1:{self.port}"
        deadline = time.time() + TIMEOUT
        while time.time() < deadline:
            with urllib.request.urlopen(f"{local}/devices/pending", timeout=5) as reply:
                pending = json.load(reply)["pending"]
            if pending:
                request = urllib.request.Request(f"{local}/devices/pending/{pending[0]['id']}/approve", data=b"{}", method="POST",
                                                 headers={"content-type": "application/json"})
                urllib.request.urlopen(request, timeout=5).close()
                return
            time.sleep(0.5)
        raise AssertionError("no phone asked to pair")

    def stop(self):
        subprocess.run([os.path.join(REPO, "tools/e2e-sandbox-stop.sh"), self.dir], env=self.env, capture_output=True)


def free_port_base():
    """A base port whose neighbours the sandbox uses (the issuer below it, the Macs and their direct listeners above) are all free."""
    import random
    import socket

    for _ in range(200):
        base = random.randrange(20000, 40000)
        try:
            for port in range(base - 1, base + 25):
                with socket.socket() as probe:
                    probe.bind(("127.0.0.1", port))
        except OSError:
            continue
        return base
    raise RuntimeError("no free port range for the sandbox")


def font(size):
    return ImageFont.truetype("/System/Library/Fonts/Menlo.ttc", size)


def ansi_cells(text):
    """Rows of (character, foreground, background, bold) from tmux's capture-pane -e output."""
    rows, fg, bg, bold, reverse = [], None, None, False, False
    for line in text.split("\n"):
        row, position = [], 0
        for match in re.finditer(r"\x1b\[([0-9;]*)m|([^\x1b]+)", line):
            if match.group(2) is not None:
                row.extend((char, fg, bg, bold, reverse) for char in match.group(2))
                continue
            codes = [int(code or 0) for code in match.group(1).split(";")]
            while codes:
                code = codes.pop(0)
                if code == 0:
                    fg, bg, bold, reverse = None, None, False, False
                elif code == 1:
                    bold = True
                elif code == 7:
                    reverse = True
                elif 30 <= code <= 37:
                    fg = BASIC_COLORS[code - 30]
                elif 90 <= code <= 97:
                    fg = BASIC_COLORS[code - 82]
                elif 40 <= code <= 47:
                    bg = BASIC_COLORS[code - 40]
                elif 100 <= code <= 107:
                    bg = BASIC_COLORS[code - 92]
                elif code == 39:
                    fg = None
                elif code == 49:
                    bg = None
                elif code in (38, 48) and codes:
                    kind = codes.pop(0)
                    color = None
                    if kind == 5 and codes:
                        color = xterm(codes.pop(0))
                    elif kind == 2 and len(codes) >= 3:
                        color = tuple(codes[:3])
                        del codes[:3]
                    if code == 38:
                        fg = color
                    else:
                        bg = color
        rows.append(row)
    return rows


def xterm(index):
    if index < 16:
        return BASIC_COLORS[index]
    if index >= 232:
        level = 8 + (index - 232) * 10
        return (level, level, level)
    index -= 16
    steps = [0, 95, 135, 175, 215, 255]
    return (steps[index // 36], steps[index // 6 % 6], steps[index % 6])


def without_machine_details(rows):
    """The screen as a visitor sees it: no sandbox band (its home folder is temporary) and no live RAM, CPU or disk of the Mac
    that shoots it. What is left is the text the TUI drew; only those two spans are cut."""
    rows = rows[1:] if rows and "".join(cell[0] for cell in rows[0]).startswith(" SANDBOX") else rows
    cut = "".join(cell[0] for cell in rows[0]).find("RAM")
    if cut >= 0:
        rows[0] = [(" ", None, None, False, False) if x >= cut else cell for x, cell in enumerate(rows[0])]
    return rows


def darkened_for_light(color):
    """A light terminal theme draws the ANSI colours darker, so a bright yellow or white that is made for a dark
    background stays readable on a light one."""
    luminance = 0.299 * color[0] + 0.587 * color[1] + 0.114 * color[2]
    if luminance <= 140:
        return color
    scale = 0.5 if luminance > 200 else 0.62
    return tuple(int(channel * scale) for channel in color)


FONT_FILES = ["/System/Library/Fonts/Menlo.ttc", "/System/Library/Fonts/Apple Symbols.ttf", "/Library/Fonts/Arial Unicode.ttf"]


class FontChain:
    """The TUI draws symbols Menlo lacks (⑂, ＋); a terminal falls back to another font for them, and so does the capture."""

    def __init__(self, size):
        self.fonts = [ImageFont.truetype(path, size) for path in FONT_FILES if os.path.exists(path)]
        self.chosen = {}

    def lacks(self, typeface, char):
        shown, notdef = typeface.getmask(char), typeface.getmask("\uFFFF")
        return shown.size == notdef.size and bytes(shown) == bytes(notdef)

    def of(self, char):
        if char not in self.chosen:
            self.chosen[char] = next((typeface for typeface in self.fonts if not self.lacks(typeface, char)), self.fonts[0])
        return self.chosen[char]


def render_terminal(ansi, appearance, path):
    palette = PALETTES[appearance]
    size = 15
    faces = FontChain(size)
    width = int(faces.fonts[0].getlength("M"))
    height = size + 5
    rows = without_machine_details(ansi_cells(ansi))
    image = Image.new("RGB", (width * 160 + 24, height * len(rows) + 24), palette["background"])
    draw = ImageDraw.Draw(image)
    for y, row in enumerate(rows):
        x = 0
        for char, fg, bg, bold, reverse in row:
            cells = 2 if unicodedata.east_asian_width(char) in "WF" else 1
            foreground, background = fg or palette["foreground"], bg
            if fg is None and bg is not None and sum(bg) < 300:
                foreground = PALETTES["dark"]["foreground"]
            if appearance == "light" and not reverse and bg is None and fg is not None:
                foreground = darkened_for_light(foreground)
            if reverse:
                foreground, background = background or palette["background"], fg or palette["foreground"]
            if background:
                draw.rectangle([12 + x * width, 12 + y * height, 12 + (x + cells) * width, 12 + (y + 1) * height], fill=background)
            if char != " ":
                draw.text((12 + x * width, 12 + y * height), char, font=faces.of(char), fill=foreground)
            x += cells
    image.save(path)


class Terminal:
    def __init__(self, sandbox, language, scratch):
        self.socket = f"alondra-demo-{os.getpid()}-{language}"
        home = os.path.join(scratch, f"tui-home-{language}")
        os.makedirs(home)
        env = {"HOME": home, "PATH": "/usr/bin:/bin", "LANG": "en_US.UTF-8", "TERM": "xterm-256color", "ALONDRA_HOME": sandbox.home,
               "ALONDRA_PORT": sandbox.port, "ALONDRA_SANDBOX": "1", "ALONDRA_LANG": language}
        command = " ".join(["env", "-i"] + [f"{k}={shlex.quote(v)}" for k, v in env.items()] + [shlex.quote(os.path.join(sandbox.dir, "bin/alondra"))])
        self.tmux("new-session", "-d", "-x", "160", "-y", "45", command)

    def tmux(self, *args):
        return subprocess.run(["tmux", "-L", self.socket, "-f", "/dev/null", *args], check=True, capture_output=True, text=True).stdout

    def screen(self, ansi=False):
        return self.tmux("capture-pane", "-p", *(["-e"] if ansi else []), "-t", "0")

    def until(self, needle, what):
        deadline = time.time() + TIMEOUT
        while needle not in (screen := self.screen()):
            if time.time() > deadline:
                raise AssertionError(f"{what}; the screen:\n{screen}")
            time.sleep(0.3)

    def keys(self, *keys):
        time.sleep(0.5)
        self.tmux("send-keys", "-t", "0", *keys)

    def stop(self):
        subprocess.run(["tmux", "-L", self.socket, "kill-server"], capture_output=True)


def shoot_tui(sandbox, out, scratch):
    for language in LANGUAGES:
        terminal = Terminal(sandbox, language, scratch)
        try:
            terminal.until("Offline mode", f"the {language} inbox never listed the plan")
            time.sleep(1)
            screens = {"inbox": terminal.screen(ansi=True)}
            terminal.keys("Tab")
            for _ in range(4):
                terminal.keys("j")
            time.sleep(1.5)
            screens["tasks"] = terminal.screen(ansi=True)
            for name, ansi in screens.items():
                for appearance in APPEARANCES:
                    render_terminal(ansi, appearance, os.path.join(out, f"tui-{name}-{language}-{appearance}.png"))
        finally:
            terminal.stop()


class Simulator:
    def __init__(self, ipa, scratch):
        self.key = f"alondra-demo-{os.getpid()}"
        self.udid = subprocess.run(["simpool", "lease", "--device", "iPhone 17 Pro", "--os", "26.3", "--key", self.key, "--ttl", "30m"],
                                   check=True, capture_output=True, text=True).stdout.strip()
        app = os.path.join(scratch, "ios-app")
        zipfile.ZipFile(ipa).extractall(app)
        bundle = os.path.join(app, "Payload", next(name for name in os.listdir(os.path.join(app, "Payload")) if name.endswith(".app")))
        subprocess.run(["xcrun", "simctl", "uninstall", self.udid, BUNDLE], capture_output=True)
        subprocess.run(["xcrun", "simctl", "keychain", self.udid, "reset"], check=True, capture_output=True)
        subprocess.run(["xcrun", "simctl", "install", self.udid, bundle], check=True)
        self.root = os.path.join(scratch, "mav")
        os.makedirs(os.path.join(self.root, ".mav"))
        with open(os.path.join(self.root, ".mav", "config.yaml"), "w") as config:
            config.write(f"target_kind: simulator\napp:\n  bundle_id: {BUNDLE}\n  process_name: AlondraiOS\nlaunch:\n  mode: already_installed\n")

    def mav(self, *args):
        env = dict(os.environ, MAV_TARGET_KIND="simulator", MAV_TARGET_UDID=self.udid)
        done = subprocess.run(["mav", *args], cwd=self.root, env=env, capture_output=True, text=True)
        if done.returncode != 0:
            raise AssertionError(f"mav {' '.join(args)}: {done.stdout}{done.stderr}")
        return done.stdout

    def launch(self, language):
        env = dict(os.environ, SIMCTL_CHILD_ALONDRA_SANDBOX="1")
        subprocess.run(["xcrun", "simctl", "launch", "--terminate-running-process", self.udid, BUNDLE,
                        "-AppleLanguages", f"({language})", "-AppleLocale", LANGUAGES[language]], env=env, check=True, capture_output=True)

    def appearance(self, appearance):
        subprocess.run(["xcrun", "simctl", "ui", self.udid, "appearance", appearance], check=True, capture_output=True)

    def ids(self):
        found = set()
        for line in self.mav("ui", "tree").splitlines():
            match = re.search(r" id=(\S+)", line)
            if match:
                found.add(match.group(1))
        return found

    def until(self, predicate, what):
        deadline = time.time() + TIMEOUT
        while not predicate(ids := self.ids()):
            if time.time() > deadline:
                raise AssertionError(what)
            time.sleep(0.3)
        return ids

    def tap(self, identifier):
        self.mav("ui", "tap", "--id", identifier)

    def capture(self, path):
        shutil.copy(self.mav("capture", "--name", "demo").split("file=")[1].split(" ")[0], path)

    def stop(self):
        subprocess.run(["xcrun", "simctl", "terminate", self.udid, BUNDLE], capture_output=True)
        subprocess.run(["xcrun", "simctl", "uninstall", self.udid, BUNDLE], capture_output=True)
        subprocess.run(["xcrun", "simctl", "keychain", self.udid, "reset"], capture_output=True)
        subprocess.run(["simpool", "release", "--key", self.key], capture_output=True)


def shoot_ios(ipa, sandbox, out, scratch):
    simulator = Simulator(ipa, scratch)
    try:
        simulator.launch("en")
        simulator.until(lambda ids: "choosePath.localNetwork" in ids or "findMac.allow" in ids or "findMac.mac" in ids, "the app never offered a way to connect")
        if "choosePath.localNetwork" in simulator.ids():
            simulator.tap("choosePath.localNetwork")
        simulator.until(lambda ids: "findMac.allow" in ids or "findMac.mac" in ids, "the app never showed pairing")
        if "findMac.allow" in simulator.ids():
            simulator.tap("findMac.allow")
        simulator.until(lambda ids: "findMac.mac" in ids, "the sandbox Mac never appeared on Bonjour")
        simulator.tap("findMac.mac")
        sandbox.approve_pairing()
        simulator.until(lambda ids: any(i.startswith("triage.need.") for i in ids), "the queue never showed")
        for language in LANGUAGES:
            simulator.launch(language)
            simulator.until(lambda ids: any(i.startswith("triage.need.") for i in ids), f"the {language} queue never showed")
            for appearance in APPEARANCES:
                simulator.appearance(appearance)
                time.sleep(1.5)
                simulator.capture(os.path.join(out, f"ios-queue-{language}-{appearance}.png"))
    finally:
        simulator.stop()


def window_id(pid):
    script = (
        "import CoreGraphics\n"
        "let pid = Int32(CommandLine.arguments[1])!\n"
        "let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as! [[String: Any]]\n"
        "for window in windows where (window[\"kCGWindowOwnerPID\"] as? Int32) == pid && (window[\"kCGWindowLayer\"] as? Int) == 0 {\n"
        "    print(window[\"kCGWindowNumber\"] as! Int); break\n"
        "}\n")
    path = os.path.join(tempfile.gettempdir(), f"alondra-demo-window-{os.getpid()}.swift")
    with open(path, "w") as file:
        file.write(script)
    try:
        return subprocess.run(["swift", path, str(pid)], capture_output=True, text=True).stdout.strip()
    finally:
        os.remove(path)


def show_review(pid):
    """Widens the window, selects the pull request in the sidebar and opens its changes, as a person would with the pointer."""
    script = f"""
tell application "System Events"
    tell (first process whose unix id is {pid})
        set frontmost to true
        set position of window 1 to {{40, 40}}
        set size of window 1 to {{1400, 900}}
        delay 1
        set primed to entire contents of window 1
        delay 1
        set rowFound to false
        set cardShown to false
        set seenRows to ""
        repeat with attempt from 1 to 6
            repeat with pane in groups of splitter group 1 of group 1 of window 1
                try
                    set sidebarRows to rows of outline 1 of scroll area 1 of pane
                    repeat with sidebarRow in sidebarRows
                        try
                            set seenRows to seenRows & (name of static text 1 of UI element 1 of sidebarRow) & " // "
                        end try
                        try
                            if (name of static text 1 of UI element 1 of sidebarRow) contains "{REVIEW_MARK}" then
                                if attempt mod 2 is 1 then
                                    select sidebarRow
                                else
                                    set rowPosition to position of sidebarRow
                                    set rowSize to size of sidebarRow
                                    click at {{(item 1 of rowPosition) + (item 1 of rowSize) / 2, (item 2 of rowPosition) + (item 2 of rowSize) / 2}}
                                end if
                                set rowFound to true
                                exit repeat
                            end if
                        end try
                    end repeat
                end try
            end repeat
            if not rowFound then
                repeat with toolbarButton in buttons of toolbar 1 of window 1
                    try
                        if (description of toolbarButton) contains "idebar" or (description of toolbarButton) contains "barra lateral" then
                            perform action "AXPress" of toolbarButton
                            exit repeat
                        end if
                    end try
                end repeat
            end if
            delay 2
            set past to false
            set seenDetail to ""
            set els to entire contents of window 1
            repeat with i from 1 to count of els
                set e to item i of els
                set r to ""
                try
                    set r to role of e
                end try
                if r is "AXSplitter" then set past to true
                if past and r is "AXStaticText" then
                    try
                        set seenDetail to seenDetail & (name of e) & " // "
                        if (name of e) contains "{REVIEW_MARK}" then set cardShown to true
                    end try
                end if
            end repeat
            if cardShown then exit repeat
        end repeat
        if not cardShown then error "the pull request's card never showed; row found: " & rowFound & "; rows: " & seenRows & "; detail: " & seenDetail
        delay 2
        set els to entire contents of window 1
        set past to false
        set seenHeading to false
        repeat with i from 1 to count of els
            set e to item i of els
            set r to ""
            try
                set r to role of e
            end try
            if r is "AXSplitter" then set past to true
            if past and r is "AXHeading" then set seenHeading to true
            if past and seenHeading and r is "AXButton" then
                perform action "AXPress" of e
                exit repeat
            end if
        end repeat
    end tell
end tell
delay 3
"""
    done = subprocess.run(["osascript", "-e", script], capture_output=True, text=True)
    if done.returncode != 0:
        raise AssertionError(f"the Mac app's window could not be driven: {done.stderr}")


def keep_changes_sheet(path):
    """The pull request's changes are a sheet over the window; the shot keeps the sheet, not the window behind it,
    whose meters show the live load of the Mac that shoots."""
    image = Image.open(path)
    width, height = image.size
    edge = 12
    image.crop((round(width * 0.105) + edge, round(height * 0.055) + edge, round(width * 0.895) - edge, height - edge)).save(path)


def shoot_mac(archive, sandbox, out, scratch):
    app_dir = os.path.join(scratch, "mac-app")
    os.makedirs(app_dir)
    subprocess.run(["ditto", "-x", "-k", archive, app_dir], check=True)
    bundle = next(os.path.join(app_dir, name) for name in os.listdir(app_dir) if name.endswith(".app"))
    executable = os.path.join(bundle, "Contents/MacOS", next(iter(os.listdir(os.path.join(bundle, "Contents/MacOS")))))
    for language in LANGUAGES:
        for appearance in APPEARANCES:
            style = ["-AppleInterfaceStyle", "Dark"] if appearance == "dark" else ["-NSRequiresAquaSystemAppearance", "YES"]
            env = dict(os.environ, ALONDRA_SANDBOX="1", ALONDRA_PORT=sandbox.port)
            process = subprocess.Popen([executable, "-AppleLanguages", f"({language})", "-AppleLocale", LANGUAGES[language], *style],
                                       env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            try:
                deadline, window = time.time() + TIMEOUT, ""
                while not window and time.time() < deadline:
                    time.sleep(2)
                    window = window_id(process.pid)
                if not window:
                    raise AssertionError("the Mac app never opened a window")
                time.sleep(4)
                show_review(process.pid)
                path = os.path.join(out, f"mac-review-{language}-{appearance}.png")
                subprocess.run(["screencapture", "-x", "-o", f"-l{window}", path], check=True)
                keep_changes_sheet(path)
            finally:
                process.terminate()
                process.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", required=True)
    parser.add_argument("--only", default="tui,ios,mac")
    parser.add_argument("--ios-ipa")
    parser.add_argument("--mac-zip")
    parser.add_argument("binaries", nargs=4)
    args = parser.parse_args()
    clients = set(args.only.split(","))
    if not (os.environ.get("SDK_NODE") and os.environ.get("SDK_HOST")):
        parser.error("SDK_NODE and SDK_HOST must name the pinned Node and the host bundle")
    if "ios" in clients and not args.ios_ipa:
        parser.error("--ios-ipa names the simulator build of //apple:AlondraiOS with --define=alondra_sandbox=1")
    if "mac" in clients and not args.mac_zip:
        parser.error("--mac-zip names the build of //apple:AlondraMac with --define=alondra_sandbox=1")
    shutil.rmtree(args.out, ignore_errors=True)
    os.makedirs(args.out)
    project = os.path.join(DEMO_PROJECTS, DEMO_PROJECT)
    if os.path.exists(project):
        parser.error(f"{project} exists: another demo run is using it, or an earlier one was cut; remove it first")
    scratch = tempfile.mkdtemp(prefix="alondra-demo-", dir=os.path.join(REPO, ".scratch") if os.path.isdir(os.path.join(REPO, ".scratch")) else None)
    made_projects = not os.path.isdir(DEMO_PROJECTS)
    os.makedirs(DEMO_PROJECTS, exist_ok=True)
    sandbox = None
    try:
        sandbox = Sandbox([os.path.abspath(binary) for binary in args.binaries], project, int(os.environ.get("SANDBOX_PORT") or free_port_base()))
        if "tui" in clients:
            shoot_tui(sandbox, args.out, scratch)
        if "ios" in clients:
            shoot_ios(args.ios_ipa, sandbox, args.out, scratch)
        if "mac" in clients:
            shoot_mac(args.mac_zip, sandbox, args.out, scratch)
        print(f"{len(os.listdir(args.out))} captures in {args.out}")
    finally:
        subprocess.run([os.path.join(REPO, "tools/e2e-sandbox-stop.sh"), project], capture_output=True)
        if made_projects:
            try:
                os.rmdir(DEMO_PROJECTS)
            except OSError:
                pass
        subprocess.run(["chmod", "-R", "u+w", scratch], capture_output=True)
        shutil.rmtree(scratch, ignore_errors=True)


if __name__ == "__main__":
    main()
