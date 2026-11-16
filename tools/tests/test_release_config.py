#!/usr/bin/env python3
"""The edit loop compiles incrementally, with clippy's state apart from rustc's; a release and CI never do."""
import os
from pathlib import Path
import re
import unittest

EXTRA = "--@rules_rust//rust/settings:extra_rustc_flags="
CLIPPY = "--@rules_rust//rust/settings:clippy_flags="
INCREMENTAL = re.compile(r"^-Cincremental=(.+)$")


def checkout():
    """The checkout as this test declares it: the `tree` targets in its data, read from its runfiles."""
    runfiles = os.environ.get("TEST_SRCDIR")
    if runfiles:
        return Path(runfiles) / os.environ.get("TEST_WORKSPACE", "_main")
    return Path(__file__).resolve().parents[2]


def settings(bazelrc, command):
    """The last value each rules_rust list setting takes for one `command` or `command:config` line."""
    values = {}
    for line in bazelrc.splitlines():
        parts = line.split(None, 1)
        if len(parts) == 2 and parts[0] == command:
            for name in (EXTRA, CLIPPY):
                if parts[1].startswith(name):
                    values[name] = [flag for flag in parts[1][len(name):].split(",") if flag]
    return values


def incremental_dirs(flags):
    return [match.group(1) for match in map(INCREMENTAL.match, flags) if match]


def plain(flags):
    return [flag for flag in flags if not INCREMENTAL.match(flag)]


class ReleaseConfigTests(unittest.TestCase):
    def setUp(self):
        root = checkout()
        self.loop = settings((root / ".bazelrc").read_text(), "build")
        self.release = settings((root / ".bazelrc").read_text(), "build:release")
        self.ci = settings((root / ".bazelrc").read_text(), "build:ci")
        self.script = (root / "deploy/brew/local-release.py").read_text()

    def test_rustc_and_clippy_compile_incrementally_into_different_directories(self):
        rustc = incremental_dirs(self.loop[EXTRA])
        clippy = incremental_dirs(self.loop[CLIPPY])
        self.assertEqual(len(rustc), 1)
        self.assertEqual(len(clippy), 1)
        self.assertNotEqual(rustc, clippy, "clippy would discard rustc's state on every check")

    def test_a_release_and_ci_compile_from_scratch_with_the_same_lints(self):
        for config in (self.release, self.ci):
            self.assertEqual(config.get(EXTRA), [])
            self.assertEqual(config.get(CLIPPY), plain(self.loop[CLIPPY]))

    def test_every_bazel_call_of_a_release_uses_the_release_config(self):
        self.assertIn("RELEASE_CONFIG = '--config=release'", self.script)
        calls = re.findall(r"run\('bazelisk'[^\n]*", self.script)
        self.assertEqual(calls, ["run('bazelisk', command, RELEASE_CONFIG, *args, capture=capture)"])

    def test_the_settings_reader_takes_the_last_value_and_an_empty_one(self):
        rc = f"build {EXTRA}-Cincremental=a\nbuild:release {EXTRA}\nbuild {EXTRA}-Cincremental=b,-Cx\n"
        self.assertEqual(settings(rc, "build")[EXTRA], ["-Cincremental=b", "-Cx"])
        self.assertEqual(settings(rc, "build:release")[EXTRA], [])


if __name__ == "__main__":
    unittest.main()
