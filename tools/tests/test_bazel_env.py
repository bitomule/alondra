#!/usr/bin/env python3
"""The pinned developer tools reach PATH without a manual step: bazelisk runs the wrapper, which installs launchers."""
import os
from pathlib import Path
import re
import stat
import unittest

ROOT = Path(os.environ["TEST_SRCDIR"]) / os.environ["TEST_WORKSPACE"]


def executable(path):
    return bool((ROOT / path).stat().st_mode & stat.S_IXUSR)


class BazelEnv(unittest.TestCase):
    def test_bazelisk_runs_the_wrapper(self):
        self.assertIn("USE_BAZEL_WRAPPER=true", (ROOT / ".bazeliskrc").read_text().splitlines())
        self.assertTrue(executable("tools/bazel"))
        self.assertTrue((ROOT / "tools/bazel").is_file(), "tools/bazel must be the wrapper file, not a package")

    def test_scripts_are_executable(self):
        for path in ["bazel/env/launcher.sh", "bazel/env/ensure.sh"]:
            self.assertTrue(executable(path), path)

    def test_the_wrapper_installs_a_launcher_for_every_tool_bazel_env_exposes(self):
        build = (ROOT / "bazel/env/BUILD.bazel").read_text()
        tools = set(re.findall(r'^\s+"(\w+)": "@', build, re.M))
        installed = set(re.search(r"for tool in ([^;]+); do", (ROOT / "tools/bazel").read_text()).group(1).split())
        self.assertEqual(tools, installed)

    def test_launchers_and_wrapper_agree_on_the_marker_that_proves_a_file_is_theirs(self):
        marker = "alondra-bazel-env-launcher"
        for path in ["bazel/env/launcher.sh", "tools/bazel"]:
            self.assertIn(marker, (ROOT / path).read_text(), path)

    def test_the_output_of_bazel_env_stays_out_of_git(self):
        ignored = (ROOT / ".gitignore").read_text().splitlines()
        self.assertIn("/bazel/env/.bazel_env", ignored)
        self.assertIn("/bazel/env/.bazel_env.stamp", ignored)


if __name__ == "__main__":
    unittest.main()
