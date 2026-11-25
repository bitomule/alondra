#!/usr/bin/env python3
"""The pinned developer tools reach PATH without a manual step: bazelisk runs the wrapper, which installs launchers."""
import os
from pathlib import Path
import re
import stat
import sys
import tempfile
import unittest

ROOT = Path(os.environ["TEST_SRCDIR"]) / os.environ["TEST_WORKSPACE"]
sys.path.insert(0, str(ROOT / "bazel/env"))
import foreign_tools  # noqa: E402


def executable(path):
    return bool((ROOT / path).stat().st_mode & stat.S_IXUSR)


class BazelEnv(unittest.TestCase):
    def test_bazelisk_runs_the_wrapper(self):
        self.assertIn("USE_BAZEL_WRAPPER=true", (ROOT / ".bazeliskrc").read_text().splitlines())
        self.assertTrue(executable("tools/bazel"))
        self.assertTrue((ROOT / "tools/bazel").is_file(), "tools/bazel must be the wrapper file, not a package")

    def test_scripts_are_executable(self):
        for path in ["bazel/env/launcher.sh", "bazel/env/ensure.sh", "bazel/env/foreign_tools.py"]:
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

    def test_the_marker_foreign_tools_looks_for_is_the_launchers(self):
        self.assertIn(foreign_tools.MARKER, (ROOT / "bazel/env/launcher.sh").read_bytes())

    def test_the_wrapper_reports_foreign_files_without_replacing_them(self):
        wrapper = (ROOT / "tools/bazel").read_text()
        self.assertIn("foreign_tools.py", wrapper)
        self.assertIn('install -m 755 "$root/bazel/env/launcher.sh" "$target"', wrapper)
        self.assertLess(wrapper.index("continue\n\t\tcmp"), wrapper.index("install -m 755"))

    def test_the_pinned_musts_version_is_read_from_tools_bzl(self):
        version = foreign_tools.pinned_musts_version(ROOT / "bazel/env/tools.bzl")
        self.assertRegex(version, r"^\d+\.\d+\.\d+$")

    def test_the_output_of_bazel_env_stays_out_of_git(self):
        ignored = (ROOT / ".gitignore").read_text().splitlines()
        self.assertIn("/bazel/env/.bazel_env", ignored)
        self.assertIn("/bazel/env/.bazel_env.stamp", ignored)


class ForeignTools(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.bin = Path(self.directory.name) / "bin"
        self.bin.mkdir()
        self.source = Path("bazel/env/launcher.sh")

    def write(self, name, content, directory=None):
        path = (directory or self.bin) / name
        path.write_bytes(content)
        path.chmod(0o755)
        return path

    def problems(self, tool, target, version, path_dirs=None, pinned="0.12.0"):
        dirs = path_dirs if path_dirs is not None else [str(self.bin)]
        return foreign_tools.problems(tool, target, self.source, pinned, dirs, lambda _: version)

    def test_a_missing_file_is_nothing_to_report(self):
        self.assertEqual(self.problems("musts", self.bin / "musts", None), [])

    def test_the_launcher_is_not_foreign(self):
        target = self.write("musts", (ROOT / "bazel/env/launcher.sh").read_bytes())
        self.assertEqual(self.problems("musts", target, "0.1.0"), [])

    def test_a_stale_binary_in_place_is_reported_with_its_version_and_the_way_to_replace_it(self):
        target = self.write("musts", b"\xcf\xfa\xed\xfe binary")
        (message,) = self.problems("musts", target, "0.10.1")
        self.assertIn(str(target), message)
        self.assertIn("Mach-O executable", message)
        self.assertIn("0.10.1", message)
        self.assertIn("0.12.0", message)
        self.assertIn(f"mv '{target}' '{target}.bak'", message)
        self.assertIn(f"install -m 755 '{self.source}' '{target}'", message)
        self.assertEqual(target.read_bytes(), b"\xcf\xfa\xed\xfe binary")

    def test_a_foreign_musts_at_the_pinned_version_is_quiet(self):
        target = self.write("musts", b"#!/bin/sh\n")
        self.assertEqual(self.problems("musts", target, "0.12.0"), [])

    def test_a_foreign_musts_with_an_unknown_version_is_reported(self):
        target = self.write("musts", b"#!/bin/sh\n")
        (message,) = self.problems("musts", target, None)
        self.assertIn("unknown", message)
        self.assertIn("script", message)

    def test_a_foreign_file_of_a_tool_without_a_pinned_version_is_reported(self):
        target = self.write("simpool", b"#!/bin/sh\n")
        (message,) = self.problems("simpool", target, None)
        self.assertIn("not the launcher", message)

    def test_a_stale_copy_ahead_of_the_launcher_on_path_is_reported(self):
        other = Path(self.directory.name) / "other"
        other.mkdir()
        target = self.write("musts", (ROOT / "bazel/env/launcher.sh").read_bytes())
        ahead = self.write("musts", b"#!/bin/sh\n", other)
        (message,) = self.problems("musts", target, "0.10.1", [str(other), str(self.bin)])
        self.assertIn(str(ahead), message)

    def test_a_stale_copy_behind_the_launcher_on_path_is_not_reported(self):
        other = Path(self.directory.name) / "other"
        other.mkdir()
        target = self.write("musts", (ROOT / "bazel/env/launcher.sh").read_bytes())
        self.write("musts", b"#!/bin/sh\n", other)
        self.assertEqual(self.problems("musts", target, "0.10.1", [str(self.bin), str(other)]), [])


if __name__ == "__main__":
    unittest.main()
