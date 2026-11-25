#!/usr/bin/env python3
"""Every default test under //tools is a target of some bazel/test check in MUSTS.yml."""
import importlib.util
import os
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("musts_scopes", Path(os.environ["MUSTS_MANIFEST"]).resolve().parents[0] / "tools" / "musts_scopes.py")
scopes = importlib.util.module_from_spec(spec)
spec.loader.exec_module(scopes)


def unchecked(default_tests, manifest):
    named = {target for check in scopes.parse_checks(manifest).values() for target in check["targets"]}
    return sorted(set(default_tests) - named)


class ToolsTestsCheckedTests(unittest.TestCase):
    def test_a_test_no_check_names_is_reported(self):
        manifest = "checks:\n  one:\n    uses: bazel/test\n    paths:\n      - \"a\"\n    with:\n      targets:\n        - //tools:a_test\n"
        self.assertEqual(unchecked(["//tools:a_test", "//tools:b_test"], manifest), ["//tools:b_test"])

    def test_every_default_tools_test_is_in_a_check(self):
        default_tests = [line for line in Path(os.environ["DEFAULT_TESTS"]).read_text().split() if line.startswith("//tools")]
        self.assertGreater(len(default_tests), 20)
        self.assertEqual(unchecked(default_tests, Path(os.environ["MUSTS_MANIFEST"]).read_text()), [])


if __name__ == "__main__":
    unittest.main()
