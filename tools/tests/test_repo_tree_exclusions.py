#!/usr/bin/env python3
"""The root package's `repo_tree()` glob never walks Bazel's symlinks or musts' transient files."""
import os
from pathlib import Path
import re
import unittest


def checkout():
    """The checkout as this test declares it: the `tree` targets in its data, read from its runfiles."""
    runfiles = os.environ.get("TEST_SRCDIR")
    if runfiles:
        return Path(runfiles) / os.environ.get("TEST_WORKSPACE", "_main")
    return Path(__file__).resolve().parents[2]


def local_state(root):
    source = (root / "tools/tree.bzl").read_text()
    block = re.search(r"LOCAL_STATE = \[(.*?)\n\]", source, re.S).group(1)
    return re.findall(r'"([^"]+)"', block)


def matches(pattern, path):
    regex = re.escape(pattern).replace(r"\*\*/", "(?:.*/)?").replace(r"\*\*", ".*").replace(r"\*", "[^/]*")
    return re.fullmatch(regex, path) is not None


def excluded(root, path):
    return any(matches(pattern, path) for pattern in local_state(root))


class RepoTreeExclusionsTest(unittest.TestCase):
    def setUp(self):
        self.root = checkout()

    def test_convenience_symlinks_live_in_a_directory_bazel_does_not_walk(self):
        flag = re.search(r"^common --symlink_prefix=(\S+)$", (self.root / ".bazelrc").read_text(), re.M)
        self.assertIsNotNone(flag, "the convenience symlinks must not land at the root of the checkout")
        directory = flag.group(1).split("/")[0]
        self.assertTrue(flag.group(1).startswith(directory + "/"), "the symlink prefix must name a directory")
        self.assertIn(directory, (self.root / ".bazelignore").read_text().splitlines())
        self.assertTrue(excluded(self.root, directory + "/bazel-out/x"))

    def test_musts_transient_files_are_excluded(self):
        for name in [".musts/.lock", ".musts/.lock.owner", ".musts/state.sqlite-wal", ".musts/bep.json"]:
            self.assertTrue(excluded(self.root, name), name)

    def test_the_pinned_tools_built_by_bazel_env_are_not_test_inputs(self):
        for name in ["bazel/env/.bazel_env/bin/musts", "bazel/env/.bazel_env.stamp"]:
            self.assertTrue(excluded(self.root, name), name)
        self.assertIn("bazel/env/.bazel_env", (self.root / ".bazelignore").read_text().splitlines())

    def test_the_validated_state_is_not_a_test_input(self):
        self.assertTrue(excluded(self.root, ".musts/ledger/root%2Fapple-app-build"))


if __name__ == "__main__":
    unittest.main()
