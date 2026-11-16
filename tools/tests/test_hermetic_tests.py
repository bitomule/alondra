#!/usr/bin/env python3
import importlib.util
import os
import tempfile
import unittest
from pathlib import Path

path = Path(__file__).resolve().parents[1] / "hermetic_tests.py"
spec = importlib.util.spec_from_file_location("hermetic_tests", path)
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)


def checkout():
    """The checkout as this test declares it: the `tree` targets in its data, read from its runfiles."""
    runfiles = os.environ.get("TEST_SRCDIR")
    if runfiles:
        return Path(runfiles) / os.environ.get("TEST_WORKSPACE", "_main")
    return Path(__file__).resolve().parents[2]


class CheckoutTest(unittest.TestCase):
    def test_every_cached_test_is_hermetic_and_every_uncached_one_says_why(self):
        self.assertEqual(guard.report(checkout()), [])


class RulesTest(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        (self.root / "tools").mkdir()
        (self.root / "app").mkdir()
        self.allow("")

    def allow(self, text):
        (self.root / guard.ALLOWLIST).write_text(text)

    def build(self, text):
        (self.root / "app" / "BUILD.bazel").write_text(text)

    def test_a_test_kept_out_of_the_cache_needs_a_reason(self):
        self.build('sh_test(\n    name = "slow_test",\n    tags = ["external"],\n)\n')
        self.assertEqual(len(guard.report(self.root)), 1)
        self.allow("//app:slow_test\tReads the network.\n")
        self.assertEqual(guard.report(self.root), [])

    def test_a_reason_for_a_cacheable_test_is_stale(self):
        self.build('sh_test(\n    name = "fast_test",\n    tags = ["manual"],\n)\n')
        self.allow("//app:fast_test\tUsed to read the network.\n")
        self.assertEqual(len(guard.report(self.root)), 1)

    def test_resolving_the_runfiles_link_back_to_the_checkout_is_refused(self):
        source = self.root / "app" / "test_checkout.py"
        source.write_text('root = (runfiles / "Makefile")' + ".resolve().parent\n")
        self.assertEqual(len(guard.report(self.root)), 1)
        source.write_text('root = Path(runfiles) / "_main"\n')
        self.assertEqual(guard.report(self.root), [])


if __name__ == "__main__":
    unittest.main()
