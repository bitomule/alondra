#!/usr/bin/env python3
"""The lockfile never records what depends on the Xcodes installed on the machine that wrote it."""
import json
import os
from pathlib import Path
import unittest

HOST_DEPENDENT = ("xcode_version", "xcode_versions", "xcode_locator")


def checkout():
    """The checkout as this test declares it: the lockfile in its data, read from its runfiles."""
    runfiles = os.environ.get("TEST_SRCDIR")
    if runfiles:
        return Path(runfiles) / os.environ.get("TEST_WORKSPACE", "_main")
    return Path(__file__).resolve().parents[2]


def host_dependent_repositories(lock):
    """The `(extension, repository)` pairs whose generated spec names an Xcode of the host."""
    found = []
    for extension, usages in lock.get("moduleExtensions", {}).items():
        for usage in usages.values():
            for repository, spec in usage.get("generatedRepoSpecs", {}).items():
                attributes = spec.get("attributes", {})
                if any(name in attributes for name in HOST_DEPENDENT):
                    found.append((extension, repository))
    return found


class ModuleLockTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.lock = json.loads((checkout() / "MODULE.bazel.lock").read_text())

    def test_no_repository_pins_an_xcode_of_the_machine_that_wrote_the_lock(self):
        self.assertEqual(
            host_dependent_repositories(self.lock),
            [],
            "rules_swift before 4.0.1 stored the Xcodes it found on the host: the next Mac with"
            " other Xcodes could not load the build",
        )

    def test_the_reader_finds_a_repository_that_names_an_xcode(self):
        lock = {"moduleExtensions": {"@@r+//e.bzl%system_sdk": {"general": {"generatedRepoSpecs": {
            "system_sdk_xcode_27_1": {"attributes": {"xcode_version": "27.1.0.27A9275"}},
            "other": {"attributes": {"names": []}},
        }}}}}
        self.assertEqual(
            host_dependent_repositories(lock),
            [("@@r+//e.bzl%system_sdk", "system_sdk_xcode_27_1")],
        )


if __name__ == "__main__":
    unittest.main()
