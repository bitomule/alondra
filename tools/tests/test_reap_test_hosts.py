#!/usr/bin/env python3
"""The cleanup selects only hosts left by tests and ends them with their group."""
import importlib.machinery
import importlib.util
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest

path = Path(__file__).resolve().parents[1] / "reap-test-hosts.py"
loader = importlib.machinery.SourceFileLoader("reap_test_hosts", str(path))
spec = importlib.util.spec_from_loader("reap_test_hosts", loader)
reap = importlib.util.module_from_spec(spec)
sys.modules["reap_test_hosts"] = reap
loader.exec_module(reap)

BAZEL = "/Volumes/BazelCache/output/" + "0" * 32 + "/execroot/_main/bazel-out/darwin_arm64-opt/bin/agent-host/alondra-host"
COPY = "/private/tmp/bt.dTKPJk/at-etQZGP/work/alondra-host-before"
PRODUCTION = "/opt/homebrew/Cellar/alondra/0.3.18/libexec/alondra/sdk/alondra-host"

# Double forks so the host is an orphan in its own group, with a child, as a cut test leaves it.
ORPHAN = """
import os, subprocess, sys
if os.fork():
    os._exit(0)
os.setsid()
child = subprocess.Popen(['/bin/sleep', '600'])
open(sys.argv[2], 'w').write(str(child.pid))
open(sys.argv[3], 'w').write(str(os.getpid()))
os.chdir(sys.argv[4])
os.execv(sys.argv[1], [sys.argv[1], '600'])
"""


def running(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


class SelectionTests(unittest.TestCase):
    def selected(self, *processes):
        return [process.pid for process in reap.left_by_tests(processes)]

    def test_an_orphan_host_of_a_bazel_output_base_or_a_test_copy_is_selected(self):
        self.assertEqual(self.selected(reap.Process(10, 1, 10, BAZEL), reap.Process(11, 1, 11, COPY)), [10, 11])

    def test_the_production_host_is_never_selected(self):
        self.assertEqual(self.selected(reap.Process(10, 1, 10, PRODUCTION)), [])

    def test_a_host_with_a_parent_is_never_selected(self):
        self.assertEqual(self.selected(reap.Process(10, 59068, 10, BAZEL), reap.Process(11, 59068, 11, COPY)), [])

    def test_a_process_that_does_not_lead_its_group_is_never_selected(self):
        self.assertEqual(self.selected(reap.Process(10, 1, 9, BAZEL)), [])

    def test_a_binary_outside_an_output_base_is_never_selected(self):
        self.assertEqual(self.selected(reap.Process(10, 1, 10, "/tmp/agent-host/alondra-host"), reap.Process(11, 1, 11, "/Users/me/alondra-host")), [])


class EndTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="rth-")).resolve()
        self.addCleanup(shutil.rmtree, self.root, ignore_errors=True)
        self.binary = self.root / ("0" * 32) / "execroot/_main/bazel-out/opt/bin/agent-host/alondra-host"
        self.binary.parent.mkdir(parents=True)
        shutil.copy("/bin/sleep", self.binary)
        subprocess.run(["/usr/bin/codesign", "--force", "--sign", "-", str(self.binary)], check=True, stderr=subprocess.DEVNULL)
        child, host = self.root / "child", self.root / "host"
        subprocess.run([sys.executable, "-c", ORPHAN, str(self.binary), str(child), str(host), str(self.root)], check=True)
        while not host.exists() or not host.read_text():
            time.sleep(0.01)
        self.child, self.host = int(child.read_text()), int(host.read_text())
        self.addCleanup(self.kill, self.host, self.child)

    @staticmethod
    def kill(*pids):
        for pid in pids:
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass

    def this_fixture(self):
        return [process for process in reap.table() if process.pid == self.host]

    def adopted(self):
        while True:
            listed = self.this_fixture()
            self.assertTrue(listed, "the host fixture runs")
            if listed[0].parent == 1 and reap.BAZEL_HOST.search(listed[0].binary):
                return listed

    def test_a_listed_host_ends_with_its_group(self):
        listed = self.adopted()
        self.assertEqual(reap.end(listed, self.this_fixture), (1, 0))
        while running(self.host) or running(self.child):
            time.sleep(0.01)

    def test_a_pid_that_stopped_being_that_host_is_left_alone(self):
        listed = self.adopted()
        other = reap.Process(self.host, 1, self.host, PRODUCTION)
        self.assertEqual(reap.end(listed, lambda: [other]), (0, 0))
        self.assertTrue(running(self.host))


if __name__ == "__main__":
    unittest.main()
