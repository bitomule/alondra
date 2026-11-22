#!/usr/bin/env python3
import importlib.util
import os
import tempfile
import unittest
from pathlib import Path

path = Path(__file__).resolve().parents[1] / "test_scratch_guard.py"
spec = importlib.util.spec_from_file_location("test_scratch_guard", path)
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)


def checkout():
    runfiles = os.environ.get("TEST_SRCDIR")
    if runfiles:
        return Path(runfiles) / os.environ.get("TEST_WORKSPACE", "_main")
    return Path(__file__).resolve().parents[2]


class TestScratchGuardTest(unittest.TestCase):
    def test_no_test_falls_back_to_or_creates_in_a_shared_temporary_folder(self):
        self.assertEqual(guard.report(checkout()), [])

    def test_the_fallbacks_that_littered_tmp_are_found(self):
        with tempfile.TemporaryDirectory() as root:
            tests = Path(root) / "crates/a/tests"
            tests.mkdir(parents=True)
            (tests / "fixture.rs").write_text(
                'let root = std::fs::canonicalize("/tmp").unwrap();\n'
                'let base = std::env::var_os("TMPDIR").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);\n'
                'std::fs::create_dir_all("/tmp/leftover").unwrap();\n'
                'let cwd = "/tmp";\n'
            )
            production = Path(root) / "crates/a/src"
            production.mkdir(parents=True)
            (production / "lib.rs").write_text('let base = PathBuf::from("/tmp");\n')
            found = guard.report(root)
        self.assertEqual([line.split(":")[1] for line in found], ["1", "2", "3"])
        self.assertTrue(all(line.startswith("crates/a/tests/fixture.rs") for line in found))


if __name__ == "__main__":
    unittest.main()
