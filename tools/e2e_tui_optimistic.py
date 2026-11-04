"""The TUI's optimistic answers, in the real TUI inside tmux against its own throwaway sandbox.

  reply     a reply written on a delivery takes it out of the queue and the detail at once; once
            its node cannot act on it (it waits for the Mac) the same delivery is back, with Accept
  frozen    with the sandbox's alondrad stopped (SIGSTOP), an answered card leaves at once, comes
            back after 30 s with a notice naming the answer, and leaves for good once alondrad
            runs again and the retried answer arrives

Every screen is kept as text (and as PNG with --png) under --captures. Never touches the real
alondrad or ~/.alondra: tools/e2e_cards.Sandbox starts and removes its own.
  tools/e2e_tui_optimistic.py --captures <dir> [--png] <alondrad> <alondra> <alondra-relay> <alondra-sandbox>
"""

import argparse
import html
import os
import re
import signal
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from e2e_cards import Node, Sandbox, Terminal, listed  # noqa: E402

DELIVERY = "Review the blurry photos mode"
QUESTION = "Pick the release channel"
REPLY = "No encontró ninguna foto borrosa; que sea una fila como Álbumes"
CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"


class Shots:
    def __init__(self, tui, directory, png):
        self.tui, self.directory, self.png, self.count = tui, directory, png, 0
        os.makedirs(directory, exist_ok=True)

    def take(self, name):
        self.count += 1
        base = os.path.join(self.directory, f"{self.count:02d}-{name}")
        with open(f"{base}.txt", "w") as file:
            file.write(self.tui.screen())
        if self.png:
            colored = self.tui.tmux("capture-pane", "-p", "-e", "-t", "0")
            with open(f"{base}.html", "w") as file:
                file.write(page(colored))
            subprocess.run([CHROME, "--headless=new", "--disable-gpu", "--hide-scrollbars", "--window-size=1290,760",
                            f"--screenshot={base}.png", f"file://{base}.html"], capture_output=True, timeout=60)
            os.remove(f"{base}.html")
        print(f"captured {base}", file=sys.stderr, flush=True)


PALETTE = ["#000000", "#cd3131", "#0dbc79", "#e5e510", "#2472c8", "#bc3fbc", "#11a8cd", "#e5e5e5",
           "#666666", "#f14c4c", "#23d18b", "#f5f543", "#3b8eea", "#d670d6", "#29b8db", "#ffffff"]


def color(index):
    if index < 16:
        return PALETTE[index]
    if index < 232:
        index -= 16
        steps = [0, 95, 135, 175, 215, 255]
        return "#%02x%02x%02x" % (steps[index // 36], steps[index // 6 % 6], steps[index % 6])
    grey = 8 + (index - 232) * 10
    return "#%02x%02x%02x" % (grey, grey, grey)


def page(colored):
    """The pane's ANSI text as a page Chrome can screenshot: colours, bold and reverse kept."""
    out, state = [], {"fg": None, "bg": None, "bold": False, "reverse": False}
    for part in re.split(r"(\x1b\[[0-9;]*m)", colored):
        if part.startswith("\x1b["):
            codes = [int(c) if c else 0 for c in part[2:-1].split(";")]
            i = 0
            while i < len(codes):
                code = codes[i]
                if code == 0:
                    state = {"fg": None, "bg": None, "bold": False, "reverse": False}
                elif code == 1:
                    state["bold"] = True
                elif code == 22:
                    state["bold"] = False
                elif code == 7:
                    state["reverse"] = True
                elif code == 27:
                    state["reverse"] = False
                elif 30 <= code <= 37 or 90 <= code <= 97:
                    state["fg"] = PALETTE[code - 30 if code < 90 else code - 82]
                elif 40 <= code <= 47 or 100 <= code <= 107:
                    state["bg"] = PALETTE[code - 40 if code < 100 else code - 92]
                elif code == 39:
                    state["fg"] = None
                elif code == 49:
                    state["bg"] = None
                elif code in (38, 48) and i + 1 < len(codes):
                    if codes[i + 1] == 5 and i + 2 < len(codes):
                        state["fg" if code == 38 else "bg"] = color(codes[i + 2])
                        i += 2
                    elif codes[i + 1] == 2 and i + 4 < len(codes):
                        state["fg" if code == 38 else "bg"] = "#%02x%02x%02x" % tuple(codes[i + 2:i + 5])
                        i += 4
                i += 1
            continue
        if not part:
            continue
        fg, bg = state["fg"] or "#d4d4d4", state["bg"] or "transparent"
        if state["reverse"]:
            fg, bg = (state["bg"] or "#1e1e1e"), (state["fg"] or "#d4d4d4")
        weight = "bold" if state["bold"] else "normal"
        out.append(f'<span style="color:{fg};background:{bg};font-weight:{weight}">{html.escape(part)}</span>')
    return ("<!doctype html><meta charset=utf-8><body style='margin:0;background:#1e1e1e'>"
            "<pre style='margin:8px;font:13px/1.2 Menlo,monospace;color:#d4d4d4'>" + "".join(out) + "</pre>")


def select(tui, need):
    """Moves the inbox selection onto the card, as a person would with j."""
    for key in ["k"] * 8 + ["j"] * 8:
        screen = tui.screen()
        selected = next((line for line in screen.splitlines() if line.startswith("│▌")), "")
        if need[:30] in selected:
            return
        tui.keys(key)
        time.sleep(0.3)
    raise AssertionError(f"could not select {need!r}:\n{tui.screen()}")


def reply_scenario(deliverer, tui, shots):
    tui.until(lambda s: listed(s, DELIVERY), "the delivery never showed")
    select(tui, DELIVERY)
    tui.until(lambda s: "Aceptar" in s, "the delivery's detail never showed Accept")
    shots.take("delivery-before-reply")
    tui.keys("r")
    tui.type(REPLY)
    shots.take("reply-written")
    tui.keys("Enter")
    started = time.time()
    screen = tui.until(lambda s: not listed(s, DELIVERY) and DELIVERY not in s, "the delivery stayed after the reply", seconds=2)
    print(f"ok: the delivery left {time.time() - started:.2f}s after Enter", file=sys.stderr, flush=True)
    assert "Enviando" not in screen and "enviando" not in screen, f"something says it is on its way:\n{screen}"
    shots.take("reply-sent-delivery-gone")
    deliverer.heard(REPLY[:20])
    time.sleep(3)
    assert not listed(tui.screen(), DELIVERY), f"the delivery came back while the node acts on the reply:\n{tui.screen()}"
    shots.take("node-acting-on-reply")
    deliverer.call(("wait_for_machine", {"resource": "memory", "reason": "Espera memoria"}))
    tui.until(lambda s: listed(s, DELIVERY), "the delivery never came back once its node stopped", seconds=20)
    select(tui, DELIVERY)
    tui.until(lambda s: "Aceptar" in s, "the delivery came back without Accept")
    shots.take("delivery-back-node-paused")
    print("ok: the same delivery is back with Accept once its node could not act on the reply", file=sys.stderr, flush=True)


def frozen_scenario(sandbox, tui, shots):
    pid = int(open(os.path.join(sandbox.home, "alondrad.pid")).read().strip())
    tui.until(lambda s: listed(s, QUESTION), "the question never showed")
    select(tui, QUESTION)
    tui.until(lambda s: "Stable" in s, "the question's detail never showed its options")
    shots.take("question-before-answer")
    os.kill(pid, signal.SIGSTOP)
    try:
        tui.keys("1")
        started = time.time()
        tui.until(lambda s: not listed(s, QUESTION), "the answered card stayed with alondrad frozen", seconds=2)
        print(f"ok: the card left {time.time() - started:.2f}s after the answer, alondrad frozen", file=sys.stderr, flush=True)
        shots.take("answered-daemon-frozen")
        time.sleep(20)
        assert not listed(tui.screen(), QUESTION), f"the card came back before 30 s:\n{tui.screen()}"
        shots.take("still-out-at-20s")
        screen = tui.until(lambda s: listed(s, QUESTION) and "aún no ha confirmado tu respuesta" in s,
                           "the card never came back with the notice", seconds=20)
        print(f"ok: back with the notice {time.time() - started:.1f}s after the answer", file=sys.stderr, flush=True)
        assert "«Stable»" in screen and QUESTION[:20] in screen, f"the notice does not name the answer:\n{screen}"
        shots.take("back-with-notice-at-30s")
    finally:
        os.kill(pid, signal.SIGCONT)
    tui.until(lambda s: not listed(s, QUESTION), "the retried answer never arrived once alondrad ran again", seconds=60)
    shots.take("retried-answer-arrived")
    print("ok: the retried answer arrived and the card left for good", file=sys.stderr, flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--captures", required=True)
    parser.add_argument("--png", action="store_true")
    parser.add_argument("binaries", nargs="+")
    args = parser.parse_args()
    signal.signal(signal.SIGTERM, lambda signum, _frame: sys.exit(128 + signum))
    sandbox = Sandbox(args.binaries, int(os.environ.get("SANDBOX_PORT", "47960")))
    nodes, tui = [], None
    try:
        asker, deliverer = Node(sandbox), Node(sandbox)
        nodes = [asker, deliverer]
        asker.call(asker.ask("question", QUESTION, ["stable", "beta"]))
        deliverer.call(("present_result", {"task_id": deliverer.task, "interaction_id": "delivery", "title": DELIVERY, "need": DELIVERY,
                                           "content": {"kind": "report", "body": "Home has a Blurry photos row."},
                                           "targets": [{"kind": "text", "text": "Home has a Blurry photos row."}], "is_final": True}))
        tui = Terminal([sandbox.alondra], sandbox.env, None)
        tui.until(lambda s: "TE ESPERA" in s, "the inbox never showed")
        shots = Shots(tui, args.captures, args.png)
        shots.take("inbox")
        reply_scenario(deliverer, tui, shots)
        frozen_scenario(sandbox, tui, shots)
        print("e2e tui optimistic: answers leave at once, come back after 30 s unconfirmed, and a replied delivery comes back when its node stops.")
    except BaseException:
        if tui:
            print(f"the screen:\n{tui.screen()}", file=sys.stderr, flush=True)
        raise
    finally:
        if tui:
            tui.stop()
        for node in nodes:
            node.stop()
        sandbox.stop()


if __name__ == "__main__":
    main()
