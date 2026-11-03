#!/usr/bin/env python3
"""build_from_source.sh removes its checkout and its fresh HOME, with a fake repository and no build."""
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

script = Path(__file__).resolve().parent / "build_from_source.sh"


class BuildFromSource(unittest.TestCase):
    def setUp(self):
        self.base = tempfile.TemporaryDirectory(dir=os.environ.get("TEST_TMPDIR"))
        self.addCleanup(self.base.cleanup)

    def repository(self, make_test):
        repo = Path(self.base.name) / "repo"
        repo.mkdir()
        (repo / "Makefile").write_text(f"build:\n\ttrue\ntest:\n\t{make_test}\n")
        git = ["git", "-C", str(repo), "-c", "user.name=t", "-c", "user.email=t@example.com"]
        subprocess.run([*git, "init", "-q"], check=True)
        subprocess.run([*git, "add", "."], check=True)
        subprocess.run([*git, "commit", "-q", "-m", "fake"], check=True)
        sha = subprocess.run([*git, "rev-parse", "HEAD"], check=True, capture_output=True, text=True).stdout.strip()
        return repo, sha

    def environment(self, **extra):
        return {**os.environ, "BUILD_FROM_SOURCE_SKIP_DISK_CHECK": "1", "TMPDIR": self.base.name, **extra}

    def run_script(self, make_test):
        repo, sha = self.repository(make_test)
        result = subprocess.run(
            [str(script), "--fresh-home", str(repo), sha],
            env=self.environment(), capture_output=True, text=True,
        )
        match = re.search(r"^scratch: (\S+) (\S+)$", result.stdout, re.M)
        self.assertIsNotNone(match, result.stdout + result.stderr)
        return result, [Path(match.group(1)), Path(match.group(2))]

    def test_a_passing_run_removes_the_checkout_and_the_fresh_home(self):
        result, folders = self.run_script("true")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for folder in folders:
            self.assertFalse(folder.exists(), folder)

    def test_a_failing_run_removes_them_too_and_keeps_the_failure(self):
        result, folders = self.run_script("false")
        self.assertNotEqual(result.returncode, 0)
        for folder in folders:
            self.assertFalse(folder.exists(), folder)

    def test_a_mac_with_user_state_is_refused_without_a_fresh_home(self):
        home = Path(self.base.name) / "home"
        home.mkdir()
        (home / ".bazelrc").write_text("")
        repo, sha = self.repository("true")
        result = subprocess.run(
            [str(script), str(repo), sha],
            env=self.environment(HOME=str(home)), capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 2)


if __name__ == "__main__":
    unittest.main()
