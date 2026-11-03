#!/usr/bin/env python3
"""What a fresh clone needs to build is in the repository, not in the maintainer's setup."""
import os
from pathlib import Path
import re
import unittest

SIGNING_FILES = {"BUILD.bazel", "repo.bzl", "signing.example.bzl"}


def runfile(*parts):
    root = Path(os.environ["TEST_SRCDIR"]) / os.environ["TEST_WORKSPACE"]
    return root.joinpath(*parts)


class PublicBuildInputs(unittest.TestCase):
    def test_no_signing_profile_is_checked_in(self):
        names = {path.name for path in runfile("signing").iterdir()}
        self.assertEqual(names, SIGNING_FILES)

    def test_the_remote_cache_is_opt_in(self):
        rc = runfile(".bazelrc").read_text()
        always_on = [
            line for line in rc.splitlines()
            if re.match(r"^(common|build|test)\s+--remote_cache=", line)
        ]
        self.assertEqual(always_on, [])
        self.assertIn("common:buildbuddy --remote_cache=", rc)

    def test_the_makefile_has_no_maintainer_values_and_no_cargo_test(self):
        makefile = runfile("Makefile").read_text()
        self.assertNotIn("David iPhone", makefile)
        self.assertNotRegex(makefile, r"(?m)^cargo-test:")
        self.assertRegex(makefile, r"(?m)^DEVICE \?=\s*$")


if __name__ == "__main__":
    unittest.main()
