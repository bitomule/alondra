#!/usr/bin/env python3
"""The checkout setup excludes its output base and preserves existing ignore entries."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SETUP = Path(__file__).resolve().parents[1] / "worktree-bazel-setup"


class SetupTest(unittest.TestCase):
    def test_committed_ignore_excludes_the_output_base(self):
        self.assertIn(b".bazel-output", (SETUP.parent.parent / ".bazelignore").read_bytes().splitlines())

    def test_ignore_contents_survive_repeated_setup_from_a_subdirectory(self):
        cases = [None, b"", b"# comment\nexperiments\n", b"experiments",
                 b".bazel-output-other\n", b".bazel-output\n", b".bazel-output",
                 b"# comment\n.bazel-output\nexperiments"]
        for original in cases:
            with self.subTest(original=original), tempfile.TemporaryDirectory(
                    dir=os.environ.get("TEST_TMPDIR")) as temporary:
                root = Path(temporary) / "checkout with spaces"
                (root / "subdir").mkdir(parents=True)
                home = Path(temporary) / "home"
                home.mkdir()
                env = dict(os.environ, HOME=str(home), GIT_CONFIG_NOSYSTEM="1",
                           GIT_CONFIG_GLOBAL=os.devnull)
                env.pop("BAZEL_SHARED_CACHE_DIR", None)
                subprocess.run(["git", "init", "--quiet", str(root)], env=env, check=True)
                ignore = root / ".bazelignore"
                if original is not None:
                    ignore.write_bytes(original)
                expected = original or b""
                if b".bazel-output" not in expected.splitlines():
                    if expected and not expected.endswith(b"\n"):
                        expected += b"\n"
                    expected += b".bazel-output\n"
                for _ in range(2):
                    subprocess.run(["sh", str(SETUP)], cwd=root / "subdir", env=env, check=True)
                    self.assertEqual(ignore.read_bytes(), expected)
                    self.assertFalse((root / "subdir/.bazelignore").exists())
                    self.assertIn("startup --output_base=" + str(root / ".bazel-output"),
                                  (root / ".bazelrc.local").read_text())


if __name__ == "__main__":
    unittest.main()
