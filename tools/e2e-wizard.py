#!/usr/bin/env python3
"""Walks `alondra`'s setup wizard on a new Mac in the e2e sandbox, end to end.

Usage: tools/e2e-wizard.py <alondrad> <alondra> <alondra-relay> <alondra-sandbox>
(other outputs bazel lists for those targets, such as clippy's .ok files, are skipped)

Starts its own sandbox (tools/e2e-sandbox.sh with NEW_MAC=1) in a fresh directory, runs
`alondra` there in a pseudo-terminal from a folder with two repos, accepts that folder and
the relay, pairs the sandbox's synthetic phone by approving it in the wizard after comparing
codes, and checks that `alondra doctor --fix` then has nothing to change. The sandbox and
every file it made are removed at the end, whatever happens.
"""

import fcntl
import json
import os
import pty
import re
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ANSI = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]")
TIMEOUT = 600


class Wizard:
    def __init__(self, argv, env, cwd):
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.chdir(cwd)
            os.execve(argv[0], argv, env)
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", 60, 240, 0, 0))
        self.seen = ""
        self.transcript = []

    def read(self, seconds):
        ready, _, _ = select.select([self.fd], [], [], seconds)
        if not ready:
            return False
        try:
            chunk = os.read(self.fd, 65536).decode("utf-8", "replace")
        except OSError:
            return None
        if not chunk:
            return None
        self.transcript.append(chunk)
        self.seen += ANSI.sub("", chunk)
        return True

    def expect(self, text, seconds=TIMEOUT):
        print(f"{time.strftime('%H:%M:%S')} waiting for {text!r}", file=sys.stderr, flush=True)
        deadline = time.time() + seconds
        while text not in self.seen:
            if time.time() > deadline or self.read(1) is None and text not in self.seen:
                raise AssertionError(f"never saw {text!r}; screen so far:\n{self.seen[-3000:]}")
        at = self.seen.index(text) + len(text)
        before, self.seen = self.seen[:at], self.seen[at:]
        return before

    def send(self, keys):
        # A prompt switches the terminal to raw mode when it starts reading, which drops keys
        # typed before; a person never types that fast.
        time.sleep(1)
        os.write(self.fd, keys.encode())

    def wait(self, seconds=60):
        deadline = time.time() + seconds
        while time.time() < deadline:
            pid, status = os.waitpid(self.pid, os.WNOHANG)
            if pid:
                return os.waitstatus_to_exitcode(status)
            self.read(0.5)
        raise AssertionError("the wizard did not exit")

    def stop(self):
        try:
            os.kill(self.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        os.close(self.fd)
        deadline = time.time() + 10
        while time.time() < deadline:
            try:
                if os.waitpid(self.pid, os.WNOHANG)[0]:
                    return
            except ChildProcessError:
                return
            time.sleep(0.2)


def stop_on_signal(signum, _frame):
    raise SystemExit(128 + signum)


def main():
    signal.signal(signal.SIGTERM, stop_on_signal)
    binaries = [os.path.abspath(p) for p in sys.argv[1:] if not p.endswith(".ok")]
    daemon, cli, relay, tool = binaries
    scratch = tempfile.mkdtemp(prefix="alondra-wizard-")
    sandbox = os.path.join(scratch, "sandbox")
    wizard = phone = None
    try:
        env = dict(os.environ, NEW_MAC="1", SANDBOX_DIR=sandbox, SANDBOX_PORT=os.environ.get("SANDBOX_PORT", "47950"))
        subprocess.run([os.path.join(REPO, "tools/e2e-sandbox.sh"), daemon, cli, relay, tool], env=env, check=True, stdout=subprocess.DEVNULL)
        manifest = json.load(open(os.path.join(sandbox, "sandbox.json")))
        new = manifest["macs"][-1]
        number, home = new["number"], new["home"]

        projects = os.path.join(scratch, "Projects")
        for name in ("app", "site"):
            os.makedirs(os.path.join(projects, name, ".git"))
        child = {k: v for k, v in os.environ.items() if not k.startswith("ALONDRA_")}
        for line in open(os.path.join(home, "env")):
            key, _, value = line.removeprefix("export ").strip().partition("=")
            child[key] = value.strip('"')
        child["TERM"] = "xterm-256color"
        child["ALONDRA_LANG"] = "en"
        alondra = os.path.join(sandbox, "bin/alondra")

        wizard = Wizard([alondra], child, projects)
        wizard.expect("setting up this Mac")
        wizard.expect("Projects folder")
        wizard.expect("2 repos")
        wizard.send("\r")
        wizard.expect("Ready for your devices")
        wizard.expect("Turn on notifications and access away from home?")
        wizard.send("\r")
        wizard.expect("Notifications and access away from home")
        wizard.expect("Waiting for your iPhone")

        phone = subprocess.Popen([os.path.join(sandbox, "bin/alondra-sandbox"), "--sandbox", sandbox, "phone", "--mac", str(number),
                                  "--name", "wizard-phone", "pair", "--direct", "--wait"], stdout=subprocess.PIPE, text=True)
        shown = re.search(r"shows code ([0-9A-Z]{6})", phone.stdout.readline())
        assert shown, "the phone did not start pairing"
        code = shown.group(1)
        asked = wizard.expect("allow only if it shows the same code.")
        assert f"{code[:3]} {code[3:]}" in asked, f"the wizard asked about another code than {code}:\n{asked[-500:]}"
        wizard.send("y")
        out, _ = phone.communicate(timeout=60)
        assert phone.returncode == 0 and "paired directly" in out, out
        wizard.send("\r")
        wizard.expect("Alondra is running. Open it on your iPhone or on this Mac.")
        assert wizard.wait() == 0
        options = open(os.path.join(home, "options.toml")).read()
        assert os.path.realpath(projects) in options, options
        assert "relay_url" in open(os.path.join(home, "service.toml")).read()
        print(ANSI.sub("", "".join(wizard.transcript)))

        again = subprocess.run([alondra, "doctor", "--fix", "--no-input"], env=child, cwd=projects, capture_output=True, text=True, timeout=TIMEOUT)
        print(again.stdout)
        assert again.returncode == 0 and "Nothing to change." in again.stdout, again.stdout + again.stderr
        print("e2e wizard: a new Mac was set up, its phone paired in the terminal, and a second run has nothing to change.")
    except BaseException:
        if wizard:
            print(f"screen so far:\n{wizard.seen[-3000:]}", file=sys.stderr, flush=True)
        raise
    finally:
        if phone and phone.poll() is None:
            phone.kill()
        if wizard:
            wizard.stop()
        print("stopping the sandbox", file=sys.stderr, flush=True)
        subprocess.run([os.path.join(REPO, "tools/e2e-sandbox-stop.sh"), sandbox], stdout=subprocess.DEVNULL, timeout=120)
        shutil.rmtree(scratch, ignore_errors=True)


if __name__ == "__main__":
    main()
