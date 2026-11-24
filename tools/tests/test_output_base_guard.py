#!/usr/bin/env python3
import importlib.util
import os
import tempfile
import unittest
from pathlib import Path

path = Path(__file__).resolve().parents[1] / "output_base_guard.py"
spec = importlib.util.spec_from_file_location("output_base_guard", path)
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)


def checkout():
    runfiles = os.environ.get("TEST_SRCDIR")
    if runfiles:
        return Path(runfiles) / os.environ.get("TEST_WORKSPACE", "_main")
    return Path(__file__).resolve().parents[2]


class OutputBaseGuardTest(unittest.TestCase):
    def test_no_file_starts_bazel_with_an_output_base_outside_the_checkout(self):
        self.assertEqual(guard.report(checkout()), [])

    def test_a_scratch_output_base_outside_the_checkout_is_found(self):
        with tempfile.TemporaryDirectory() as root:
            (Path(root) / "docs").mkdir()
            (Path(root) / "docs/notes.md").write_text(
                "bazelisk --output_base=$HOME/.cache/flaky-ob test //a:b\n"
                'bazelisk --output_base="/tmp/ob" test //a:b\n'
                'bazelisk --output_base="$PWD/.scratch/recovery" test //a:b\n'
            )
            (Path(root) / "Makefile").write_text("build:\n\tbazelisk --output_base=~/ob test x\n")
            found = guard.report(root)
        self.assertEqual(
            [line.rsplit(": starts", 1)[0] for line in found],
            ["Makefile:2", "docs/notes.md:1", "docs/notes.md:2"],
        )


if __name__ == "__main__":
    unittest.main()
