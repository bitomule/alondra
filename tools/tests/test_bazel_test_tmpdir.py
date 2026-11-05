#!/usr/bin/env python3
"""The bazel test TMPDIR wrapper leaves nothing of its own behind, however the test ends."""
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time
import unittest

wrapper = Path(__file__).resolve().parents[1] / "test-tmpdir.sh"

READ_ONLY_TREE = 'mkdir -p "$TMPDIR/cache/rules" && touch "$TMPDIR/cache/rules/BUILD" && chmod 555 "$TMPDIR/cache/rules"'


# Starts a process in its own group, as a host runs, and waits until it told its pid.
LEFT_RUNNING = """cd "$TMPDIR" && echo '{{}}' > {lock} && python3 -c '
import os, sys
os.setpgid(0, 0)
open(sys.argv[1], "w").write(str(os.getpid()))
os.execv("/bin/sleep", ["sleep", "600"])
' "{pid}" < {lock} >/dev/null 2>&1 &
until [ -s "{pid}" ]; do sleep 0.01; done
"""


def running(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


def end(pid):
    if pid.exists() and pid.read_text():
        try:
            os.kill(int(pid.read_text()), signal.SIGKILL)
        except ProcessLookupError:
            pass


def remove(path):
    for folder, _, _ in os.walk(path):
        os.chmod(folder, 0o700)
    subprocess.run(["rm", "-rf", str(path)], check=True)


class BazelTestTmpdirTest(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="btt-"))
        self.addCleanup(remove, self.root)

    def run_wrapped(self, script):
        env = dict(os.environ, BAZEL_TEST_TMPDIR_ROOT=str(self.root))
        return subprocess.run([str(wrapper), "/bin/sh", "-c", script], env=env).returncode

    def test_a_passing_test_leaves_nothing_even_read_only_folders(self):
        self.assertEqual(self.run_wrapped(READ_ONLY_TREE), 0)
        self.assertEqual(list(self.root.iterdir()), [])

    def test_a_failing_test_leaves_nothing_and_keeps_its_exit_code(self):
        self.assertEqual(self.run_wrapped(READ_ONLY_TREE + " && exit 3"), 3)
        self.assertEqual(list(self.root.iterdir()), [])

    def test_a_test_cut_by_sigterm_leaves_nothing(self):
        started = self.root.parent / f"{self.root.name}.started"
        self.addCleanup(started.unlink, missing_ok=True)
        env = dict(os.environ, BAZEL_TEST_TMPDIR_ROOT=str(self.root))
        script = f'{READ_ONLY_TREE} && touch "{started}" && exec sleep 600'
        process = subprocess.Popen([str(wrapper), "/bin/sh", "-c", script], env=env)
        self.addCleanup(process.kill)
        while not started.exists():
            self.assertIsNone(process.poll(), "the wrapped test ended before it started")
            time.sleep(0.01)
        process.send_signal(signal.SIGTERM)
        self.assertEqual(process.wait(timeout=60), 143)
        self.assertEqual(list(self.root.iterdir()), [])

    def left_running(self, lock):
        pid = self.root.parent / f"{self.root.name}.pid"
        self.addCleanup(pid.unlink, missing_ok=True)
        self.addCleanup(end, pid)
        return pid, LEFT_RUNNING.format(lock=lock, pid=pid)

    def assert_ended(self, pid):
        while running(int(pid.read_text())):
            time.sleep(0.01)
        self.assertEqual(list(self.root.iterdir()), [])

    def test_a_passing_test_that_leaves_a_host_fails_and_the_host_ends(self):
        pid, script = self.left_running("host.lock")
        self.assertEqual(self.run_wrapped(script), 1)
        self.assert_ended(pid)

    def test_a_failing_test_that_leaves_a_host_keeps_its_exit_code_and_the_host_ends(self):
        pid, script = self.left_running("host.lock")
        self.assertEqual(self.run_wrapped(script + "exit 3"), 3)
        self.assert_ended(pid)

    def test_a_host_of_a_test_cut_by_sigterm_ends(self):
        pid, script = self.left_running("host.lock")
        env = dict(os.environ, BAZEL_TEST_TMPDIR_ROOT=str(self.root))
        process = subprocess.Popen([str(wrapper), "/bin/sh", "-c", script + "exec sleep 600"], env=env)
        self.addCleanup(process.kill)
        while not pid.exists() or not pid.read_text():
            self.assertIsNone(process.poll(), "the wrapped test ended before it started")
            time.sleep(0.01)
        process.send_signal(signal.SIGTERM)
        self.assertEqual(process.wait(timeout=60), 143)
        self.assert_ended(pid)

    def test_a_process_that_is_no_host_ends_without_failing_the_test(self):
        pid, script = self.left_running("input")
        self.assertEqual(self.run_wrapped(script), 0)
        self.assert_ended(pid)

    def test_folders_of_dead_wrappers_go_and_others_stay(self):
        dead = subprocess.Popen(["true"])
        dead.wait()
        orphan = self.root / "bt.orphan"
        (orphan / "cache/rules").mkdir(parents=True)
        (orphan / ".owner").write_text(f"{dead.pid}\n")
        (orphan / "cache/rules").chmod(0o555)
        alive = self.root / "bt.alive"
        alive.mkdir()
        (alive / ".owner").write_text(f"{os.getpid()}\n")
        unowned = self.root / "bt.unowned"
        unowned.mkdir()

        self.assertEqual(self.run_wrapped("true"), 0)
        self.assertEqual(sorted(p.name for p in self.root.iterdir()), ["bt.alive", "bt.unowned"])


if __name__ == "__main__":
    unittest.main()
