#!/usr/bin/env python3
"""Every build but a release and CI compiles the repository's libraries and binaries incrementally, never
sandboxed, with clippy's state apart from rustc's; third-party crates keep their cache."""
import os
from pathlib import Path
import re
import unittest

EXTRA = "--@rules_rust//rust/settings:extra_rustc_flags="
CLIPPY = "--@rules_rust//rust/settings:clippy_flags="
MODIFY = "--modify_execution_info="
SWITCH = "--//bazel/incremental="
INCREMENTAL = re.compile(r"^-Cincremental=(.+)$")
STATE = re.compile(r'^INCREMENTAL_STATE = "(.+)"$', re.M)


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


def execution_info(bazelrc, command):
    """The last `--modify_execution_info` of one `command` or `command:config`, as a regex-to-tags map."""
    value = ""
    for line in bazelrc.splitlines():
        parts = line.split(None, 1)
        if len(parts) == 2 and parts[0] == command and parts[1].startswith(MODIFY):
            value = parts[1][len(MODIFY):]
    return dict(rule.rsplit("=", 1) for rule in re.split(r",(?=\^)", value) if rule)


def switch(bazelrc, command):
    """The last value of the incremental switch for one `command` or `command:config`, if any."""
    value = None
    for line in bazelrc.splitlines():
        parts = line.split(None, 1)
        if len(parts) == 2 and parts[0] == command and parts[1].startswith(SWITCH):
            value = parts[1][len(SWITCH):]
    return value


def incremental_dirs(flags):
    return [match.group(1) for match in map(INCREMENTAL.match, flags) if match]


def plain(flags):
    return [flag for flag in flags if not INCREMENTAL.match(flag)]


class ReleaseConfigTests(unittest.TestCase):
    def setUp(self):
        root = checkout()
        self.rc = (root / ".bazelrc").read_text()
        self.default = settings(self.rc, "build")
        self.release = settings(self.rc, "build:release")
        self.ci = settings(self.rc, "build:ci")
        self.rustc_state = STATE.search((root / "bazel/rules/rust.bzl").read_text()).group(1)
        self.script = (root / "deploy/brew/local-release.py").read_text()

    def test_clippy_keeps_its_incremental_state_apart_from_rustc(self):
        clippy = incremental_dirs(self.default[CLIPPY])
        self.assertEqual(len(clippy), 1)
        self.assertNotEqual(clippy[0], self.rustc_state, "clippy would discard rustc's state on every check")
        self.assertTrue(self.default[CLIPPY][-1].startswith("-Cincremental="), "clippy's state must come after the crate's")

    def test_a_release_and_ci_compile_from_scratch_with_the_same_lints(self):
        self.assertIsNone(switch(self.rc, "build"), "the switch defaults to incremental in its BUILD file")
        self.assertNotIn(EXTRA, self.default)
        for config, name in ((self.release, "build:release"), (self.ci, "build:ci")):
            self.assertEqual(switch(self.rc, name), "false")
            self.assertEqual(config.get(EXTRA), [])
            self.assertEqual(config.get(CLIPPY), plain(self.default[CLIPPY]))

    def test_incremental_compiles_never_run_sandboxed(self):
        for mnemonic in ("Rustc", "Clippy"):
            self.assertIn(f"build --strategy={mnemonic}=worker,local\n", self.rc)

    def test_third_party_crates_keep_their_cache_in_every_configuration(self):
        lists = {config: execution_info(self.rc, config) for config in ("common", "common:release", "common:ci")}
        for config, rules in lists.items():
            for rule in rules:
                self.assertNotRegex(rule, "Rustc|Clippy", f"{config} would drop the third-party crates' cache")
        self.assertEqual(lists["common"], lists["common:release"])
        self.assertEqual(lists["common"], lists["common:ci"])

    def test_every_bazel_call_of_a_release_uses_the_release_config(self):
        self.assertIn("RELEASE_CONFIG = '--config=release'", self.script)
        calls = re.findall(r"run\('bazelisk'[^\n]*", self.script)
        self.assertEqual(calls, ["run('bazelisk', command, RELEASE_CONFIG, *args, capture=capture)"])

    def test_the_settings_reader_takes_the_last_value_and_an_empty_one(self):
        rc = f"build {EXTRA}-Cincremental=a\nbuild:release {EXTRA}\nbuild {EXTRA}-Cincremental=b,-Cx\n"
        self.assertEqual(settings(rc, "build")[EXTRA], ["-Cincremental=b", "-Cx"])
        self.assertEqual(settings(rc, "build:release")[EXTRA], [])

    def test_the_execution_info_reader_splits_rules_and_takes_the_last_value(self):
        rc = f"common {MODIFY}^(A|B)$=+x,^(C)$=+y\n"
        self.assertEqual(execution_info(rc, "common"), {"^(A|B)$": "+x", "^(C)$": "+y"})
        rc += f"common:ci {MODIFY}^(A|B)$=+x\ncommon {MODIFY}^(D)$=+z\n"
        self.assertEqual(execution_info(rc, "common"), {"^(D)$": "+z"})
        self.assertEqual(execution_info(rc, "common:ci"), {"^(A|B)$": "+x"})


if __name__ == "__main__":
    unittest.main()
