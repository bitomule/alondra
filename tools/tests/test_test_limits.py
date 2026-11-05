#!/usr/bin/env python3
"""Every test in the checkout declares a limit, and the guard catches each way of missing one."""
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest

path = Path(__file__).resolve().parents[1] / "test_limits.py"
spec = importlib.util.spec_from_file_location("test_limits", path)
limits = importlib.util.module_from_spec(spec)
spec.loader.exec_module(limits)

BAZELRC = "\n".join(limits.REQUIRED_FLAGS) + "\n"


def checkout():
    """The checkout this test runs from: Bazel's runfiles link the Makefile back to it."""
    runfiles = os.environ.get("TEST_SRCDIR")
    if runfiles:
        makefile = Path(runfiles) / os.environ.get("TEST_WORKSPACE", "_main") / "Makefile"
        return makefile.resolve().parent
    return Path(__file__).resolve().parents[2]


class CheckoutTest(unittest.TestCase):
    def test_every_test_in_the_checkout_has_a_limit(self):
        problems = limits.report(checkout())
        self.assertEqual(problems, [], "\n" + "\n".join(problems))


class GuardTest(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        (self.root / "tools").mkdir()
        (self.root / "app").mkdir()
        (self.root / ".bazelrc").write_text(BAZELRC)
        self.allow("")

    def allow(self, text):
        (self.root / "tools" / limits.ALLOWLIST).write_text(text)

    def problems(self, build):
        (self.root / "app" / "BUILD.bazel").write_text(build)
        return limits.report(self.root)

    def test_a_test_with_a_size_or_a_timeout_passes(self):
        self.assertEqual(self.problems('sh_test(name = "a", size = "small")\nswift_test(name = "b", timeout = "short")\n'), [])

    def test_a_test_with_neither_is_named(self):
        self.assertEqual(self.problems('rust_test(name = "a")\n'), ["//app:a: declares neither size nor timeout"])

    def test_a_test_made_in_a_comprehension_is_seen(self):
        self.assertEqual(len(self.problems('[sh_test(name = n) for n in ["a"]]\n')), 1)

    def test_a_macro_forwards_its_callers_limit_or_declares_one(self):
        (self.root / "app" / "rules.bzl").write_text('def a(**kwargs):\n    native.sh_test(name = "a", **kwargs)\n')
        self.assertEqual(self.problems(""), [])
        (self.root / "app" / "rules.bzl").write_text('def a():\n    native.sh_test(name = "a")\n')
        self.assertEqual(self.problems(""), ["//app:a: declares neither size nor timeout"])

    def test_a_library_is_not_a_test(self):
        self.assertEqual(self.problems('rust_library(name = "a")\ntest_suite(name = "b")\n'), [])

    def test_a_generous_limit_needs_a_reason(self):
        self.assertEqual(len(self.problems('sh_test(name = "a", timeout = "long")\n')), 1)
        self.assertEqual(len(self.problems('sh_test(name = "a", size = "large")\n')), 1)

    def test_a_large_test_with_a_short_timeout_is_not_generous(self):
        self.assertEqual(self.problems('sh_test(name = "a", size = "large", timeout = "short")\n'), [])

    def test_a_manual_test_keeps_a_generous_limit(self):
        self.assertEqual(self.problems('sh_test(name = "a", timeout = "long", tags = ["manual"])\n'), [])

    def test_a_listed_test_keeps_a_generous_limit(self):
        self.allow("//app:a\tBoots a simulator twice\n")
        self.assertEqual(self.problems('sh_test(name = "a", timeout = "long")\n'), [])

    def test_a_listed_test_needs_a_reason(self):
        self.allow("//app:a\n")
        self.assertIn(f"//app:a: entry in {limits.ALLOWLIST} has no reason", self.problems('sh_test(name = "a", timeout = "long")\n'))

    def test_an_entry_nothing_needs_is_refused(self):
        self.allow("//app:a\tBoots a simulator twice\n")
        self.assertIn(f"//app:a: entry in {limits.ALLOWLIST} is no longer needed", self.problems('sh_test(name = "a", size = "small")\n'))

    def test_the_bazelrc_flags_are_required(self):
        (self.root / ".bazelrc").write_text("")
        self.assertEqual(len(self.problems("")), len(limits.REQUIRED_FLAGS))


if __name__ == "__main__":
    unittest.main()
