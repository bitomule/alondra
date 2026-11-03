#!/usr/bin/env python3
"""Every rust_test goes through low_opt_rust_test, except the ones that measure speed."""
import os
from pathlib import Path
import re
import unittest

SPEED_TESTS = {"crates/core/BUILD.bazel:views_budget_test"}
PLAIN_RUST_TEST = re.compile(r'^\s*(?:\[)?rust_test\(\s*name\s*=\s*"([^"]+)"', re.MULTILINE)


def checkout():
    """The checkout this test runs from: Bazel's runfiles link the Makefile back to it."""
    runfiles = os.environ.get("TEST_SRCDIR")
    if runfiles:
        makefile = Path(runfiles) / os.environ.get("TEST_WORKSPACE", "_main") / "Makefile"
        return makefile.resolve().parent
    return Path(__file__).resolve().parents[2]


def plain_rust_tests(root):
    found = set()
    for directory in ("crates", "tools", "agent-host", "tests"):
        for build in sorted((root / directory).rglob("BUILD.bazel")):
            for name in PLAIN_RUST_TEST.findall(build.read_text()):
                found.add(f"{build.relative_to(root)}:{name}")
    return found


class LowOptRustTests(unittest.TestCase):
    def test_only_speed_tests_use_the_plain_rust_test(self):
        self.assertEqual(plain_rust_tests(checkout()), SPEED_TESTS)

    def test_a_plain_rust_test_is_detected(self):
        self.assertTrue(PLAIN_RUST_TEST.findall('rust_test(\n    name = "x_test",'))
        self.assertFalse(PLAIN_RUST_TEST.findall('low_opt_rust_test(\n    name = "x_test",'))


if __name__ == "__main__":
    unittest.main()
