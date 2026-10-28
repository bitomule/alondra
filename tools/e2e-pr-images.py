#!/usr/bin/env python3
"""Shows the images of a pull request's description in the real TUI against the e2e sandbox.

Usage: tools/e2e-pr-images.py <alondrad> <alondra> <alondra-relay> <alondra-sandbox>

The sandbox's scripted node commits a screenshot to its branch, opens and registers a PR whose
description cites it, one image that is not in the branch and one of a repository GitHub does not
show. Presenting it is refused, naming the missing image; without it the PR is presented. The TUI
then shows the screenshot as an image the daemon brought from the fake GitHub, `v` opens that copy
with the screenshot's bytes, and the other image says why it is not shown. E2E_CAPTURES=<dir> keeps
what the TUI showed; the sandbox and every other file it made are removed at the end.
"""

import importlib.util
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
_spec = importlib.util.spec_from_file_location("e2e_doc_review", os.path.join(REPO, "tools/e2e-doc-review.py"))
review = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(review)
_spec = importlib.util.spec_from_file_location("e2e_doc_links", os.path.join(REPO, "tools/e2e-doc-links.py"))
links = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(links)

NUMBER = 7
NEED = "Review the storage screens"
SHOWN = "![storage areas](docs/pr/shot.png)\n\n![](https://github.com/secret/repo/blob/main/a.png)\n"
MISSING = "![old screen](docs/pr/gone.png)\n\n"


def describe(sandbox, body):
    """Writes the PR's description on the fake GitHub, whole, as a push of `gh pr edit` would."""
    path = os.path.join(sandbox, "fakes", "github", "prs.json")
    prs = json.load(open(path))
    prs[str(NUMBER)]["body"] = f"The storage screens:\n\n{body}"
    with open(path + ".partial", "w") as file:
        json.dump(prs, file)
    os.replace(path + ".partial", path)


def steps(node, *steps):
    return node.tool("fake", "node", node.task, "call", "--step", json.dumps(list(steps))).splitlines()


def main():
    signal.signal(signal.SIGTERM, lambda signum, _frame: sys.exit(128 + signum))
    binaries = [os.path.abspath(p) for p in sys.argv[1:] if not p.endswith(".ok")]
    daemon, cli, relay, tool = binaries
    scratch = os.path.realpath(tempfile.mkdtemp(prefix="alondra-pr-images-", dir="/private/tmp"))
    sandbox = os.path.join(scratch, "sandbox")
    log = os.path.join(scratch, "opened.txt")
    tui = None
    try:
        env = dict(os.environ, SANDBOX_DIR=sandbox, SANDBOX_PORT=os.environ.get("SANDBOX_PORT", "47960"), MACS="1", FAKES="1")
        subprocess.run([os.path.join(REPO, "tools/e2e-sandbox.sh"), daemon, cli, relay, tool], env=env, check=True, stdout=subprocess.DEVNULL)
        home = json.load(open(os.path.join(sandbox, "sandbox.json")))["macs"][0]["home"]
        child = {k: v for k, v in os.environ.items() if not k.startswith("ALONDRA_")}
        for line in open(os.path.join(home, "env")):
            key, _, value = line.removeprefix("export ").strip().partition("=")
            child[key] = value.strip('"')
        child.update(TERM="xterm-256color", ALONDRA_LANG="es", ALONDRA_SANDBOX="1", ALONDRA_SANDBOX_OPENED=log)
        node = review.Node(sandbox, child)
        worktree = node.session()["cwd"]
        os.makedirs(os.path.join(worktree, "docs", "pr"), exist_ok=True)
        with open(os.path.join(worktree, "docs", "pr", "shot.png"), "wb") as file:
            file.write(links.PNG)
        subprocess.run(["git", "add", "-A"], cwd=worktree, check=True)
        subprocess.run(["git", "commit", "-qm", "Storage screens"], cwd=worktree, check=True, env=child)
        done = steps(node, {"do": "push"},
                     {"do": "open_pr", "number": NUMBER}, {"do": "register_pr", "number": NUMBER})
        assert all(line.startswith("ok ") for line in done), "\n".join(done)

        describe(sandbox, MISSING + SHOWN)
        refused = "\n".join(steps(node, {"do": "deliver", "need": NEED, "pr": NUMBER}))
        assert refused.startswith("refused "), refused
        assert "pr_images_missing" in refused and "docs/pr/gone.png (not in the branch" in refused, refused
        assert "shot.png" not in refused and "secret/repo" not in refused, refused
        print("ok: a PR citing an image missing from its branch is refused, naming it", file=sys.stderr, flush=True)

        describe(sandbox, SHOWN)
        presented = "\n".join(steps(node, {"do": "deliver", "need": NEED, "pr": NUMBER}))
        assert presented.startswith("ok "), presented

        captures = os.environ.get("E2E_CAPTURES") or os.path.join(scratch, "captures")
        os.makedirs(captures, exist_ok=True)
        tui = review.Terminal([os.path.join(sandbox, "bin/alondra")], child, captures)
        tui.expect(NEED)
        tui.send("Enter")
        screen = tui.expect("📷 storage areas")
        assert "📷 storage areas (" not in screen, screen
        assert "a.png — GitHub no da acceso a secret/repo (github.com)" in screen, screen
        tui.capture("pr-description")
        print("ok: the description shows the image the daemon brought and why the other is not shown", file=sys.stderr, flush=True)

        tui.send("/")
        tui.type("storage areas")
        tui.send("Enter")
        tui.send("v")
        copy = links.opened(log, 1)[0]
        try:
            assert copy.endswith(".png") and open(copy, "rb").read() == links.PNG, copy
        finally:
            shutil.rmtree(os.path.dirname(copy), ignore_errors=True)
        tui.capture("opened-image")
        print("ok: v opens the copy with the screenshot's bytes", file=sys.stderr, flush=True)
        if os.environ.get("E2E_CAPTURES"):
            print(f"TUI captures in {captures}", file=sys.stderr, flush=True)
        print("e2e pr images: the TUI showed the PR description's images from the daemon and why one was not shown.")
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
