#!/usr/bin/env python3
"""Opens the links and images of a README-like document in the real TUI against the e2e sandbox.

Usage: tools/e2e-doc-links.py <alondrad> <alondra> <alondra-relay> <alondra-sandbox>

The sandbox's scripted node writes a document with Markdown and inline HTML links and images and
asks for its review, as tools/e2e-doc-review.py does. `alondra` runs in tmux with
`ALONDRA_SANDBOX_OPENED`, where the sandbox TUI writes what it would hand the Mac's `open`. The test
checks the HTML reads as text and image markers, that `v` opens the one link of a block, that a
block with several shows the numbered list and opens the one picked, that an image the node had in
its worktree was copied into Alondra's files folder and `v` opens that copy, and that an image it
could not copy is refused with its reason. E2E_CAPTURES=<dir> keeps what the TUI showed at each step there; the
sandbox and every other file it made are removed at the end, whatever happens.
"""

import importlib.util
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
_spec = importlib.util.spec_from_file_location("e2e_doc_review", os.path.join(REPO, "tools/e2e-doc-review.py"))
review = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(review)

DOCUMENT = """<!-- badges -->
<p align="center">
  <img src="https://alondra.dev/logo.png" alt="Alondra logo" width=120>
</p>

<h1 align=center>Alondra</h1>

<p align="center">
  <a href="https://alondra.dev/docs">Docs</a> ·
  <a href='https://github.com/example/alondra'>Code</a>
</p>

Read [the install guide](https://alondra.dev/install) first.

A screenshot: ![main screen](docs/main.png)

Not in the worktree: ![old screen](docs/gone.png)
"""

PNG = bytes.fromhex("89504e470d0a1a0a0000000d4948445200000001000000010806000000"
                    "1f15c4890000000d49444154789c6360f80f0000010100005b0a2e6f0000000049454e44ae426082")


def opened(path, count):
    deadline = time.time() + review.TIMEOUT
    while True:
        lines = open(path).read().splitlines() if os.path.exists(path) else []
        if len(lines) >= count:
            return lines
        assert time.time() < deadline, f"the TUI opened {lines}, expected {count} entries"
        time.sleep(0.2)


def main():
    signal.signal(signal.SIGTERM, lambda signum, _frame: sys.exit(128 + signum))
    binaries = [os.path.abspath(p) for p in sys.argv[1:] if not p.endswith(".ok")]
    daemon, cli, relay, tool = binaries
    scratch = os.path.realpath(tempfile.mkdtemp(prefix="alondra-links-", dir="/private/tmp"))
    sandbox = os.path.join(scratch, "sandbox")
    log = os.path.join(scratch, "opened.txt")
    tui = None
    try:
        env = dict(os.environ, SANDBOX_DIR=sandbox, SANDBOX_PORT=os.environ.get("SANDBOX_PORT", "47950"), MACS="1", FAKES="1")
        subprocess.run([os.path.join(REPO, "tools/e2e-sandbox.sh"), daemon, cli, relay, tool], env=env, check=True, stdout=subprocess.DEVNULL)
        home = json.load(open(os.path.join(sandbox, "sandbox.json")))["macs"][0]["home"]
        child = {k: v for k, v in os.environ.items() if not k.startswith("ALONDRA_")}
        for line in open(os.path.join(home, "env")):
            key, _, value = line.removeprefix("export ").strip().partition("=")
            child[key] = value.strip('"')
        child.update(TERM="xterm-256color", ALONDRA_LANG="es", ALONDRA_SANDBOX="1", ALONDRA_SANDBOX_OPENED=log)
        node = review.Node(sandbox, child)
        worktree = node.session()["cwd"]
        os.makedirs(os.path.join(worktree, "docs"), exist_ok=True)
        with open(os.path.join(worktree, "docs/main.png"), "wb") as file:
            file.write(PNG)
        node.call(node.document("readme-r1", DOCUMENT, first=True), *node.ask("links-1", "readme-r1", "Review the README"))
        captures = os.environ.get("E2E_CAPTURES") or os.path.join(scratch, "captures")
        os.makedirs(captures, exist_ok=True)
        tui = review.Terminal([os.path.join(sandbox, "bin/alondra")], child, captures)
        tui.expect("Review the README")
        tui.send("Enter")
        screen = tui.expect("📷 Alondra logo (alondra.dev)")
        for raw in ("<p", "<img", "<a ", "<h1", "<!--"):
            assert raw not in screen, f"{raw} shows as raw HTML:\n{screen}"
        assert "Docs (alondra.dev) · Code (github.com)" in screen, screen
        assert "📷 main screen" in screen and "📷 main screen (" not in screen, screen
        assert "📷 old screen (imagen local, no disponible aquí)" in screen, screen
        tui.capture("readme")

        tui.send("/")
        tui.type("install guide")
        tui.send("Enter")
        tui.expect("v abrir")
        tui.send("v")
        assert opened(log, 1) == ["https://alondra.dev/install"]
        tui.expect("Abierto «the install guide»")
        tui.capture("opened-one-link")
        print("ok: v opens the one link of the selected block", file=sys.stderr, flush=True)

        tui.send("/")
        tui.type("Docs")
        tui.send("Enter")
        tui.send("v")
        tui.expect("1 ↗ Docs")
        tui.expect("2 ↗ Code")
        tui.capture("several-targets")
        tui.send("2")
        assert opened(log, 2)[1] == "https://github.com/example/alondra"
        print("ok: a block with several links lists them and opens the one picked", file=sys.stderr, flush=True)

        tui.send("/")
        tui.type("screenshot")
        tui.send("Enter")
        tui.send("v")
        copy = opened(log, 3)[2]
        try:
            assert copy.endswith(".png") and open(copy, "rb").read() == PNG, copy
        finally:
            shutil.rmtree(os.path.dirname(copy), ignore_errors=True)
        tui.capture("worktree-image")
        print("ok: an image from the node's worktree was copied and opens from the daemon", file=sys.stderr, flush=True)

        tui.send("/")
        tui.type("Not in the worktree")
        tui.send("Enter")
        tui.send("v")
        tui.expect("no se puede abrir desde aquí")
        tui.capture("local-image")
        time.sleep(1)
        assert len(opened(log, 3)) == 3, "the local image was handed to open"
        print("ok: an image it could not copy says why it does not open", file=sys.stderr, flush=True)
        if os.environ.get("E2E_CAPTURES"):
            print(f"TUI captures in {captures}", file=sys.stderr, flush=True)
        print("e2e doc links: the TUI showed the HTML as text and images and opened what each block links to.")
    except BaseException:
        if tui:
            print(f"the screen:\n{tui.screen()}", file=sys.stderr, flush=True)
        raise
    finally:
        if tui:
            tui.stop()
        print("stopping the sandbox", file=sys.stderr, flush=True)
        subprocess.run([os.path.join(REPO, "tools/e2e-sandbox-stop.sh"), sandbox], stdout=subprocess.DEVNULL, timeout=120)
        shutil.rmtree(scratch, ignore_errors=True)


if __name__ == "__main__":
    main()
