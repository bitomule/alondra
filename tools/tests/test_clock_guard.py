#!/usr/bin/env python3
"""The daemon's production code reads the clock only through core's clock, and the guard names each way of not doing so."""
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest

path = Path(__file__).resolve().parents[1] / "clock_guard.py"
spec = importlib.util.spec_from_file_location("clock_guard", path)
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)


def checkout():
    """The checkout as this test declares it: the `tree` targets in its data, read from its runfiles."""
    runfiles = os.environ.get("TEST_SRCDIR")
    if runfiles:
        return Path(runfiles) / os.environ.get("TEST_WORKSPACE", "_main")
    return Path(__file__).resolve().parents[2]


class CheckoutTest(unittest.TestCase):
    def test_no_daemon_crate_reads_the_system_clock_or_waits_on_its_own(self):
        found = guard.problems(checkout())
        self.assertEqual(found, [], "\n" + "\n".join(found))


class GuardTest(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)

    def write(self, relative, text):
        target = self.root / "crates" / "daemon-engine" / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)

    def test_a_direct_reading_of_the_clock_is_named(self):
        for line in [
            "let a = std::time::Instant::now();",
            "let a = Instant::now();",
            "let a = std::time::SystemTime::now();",
            "std::thread::sleep(d);",
            "tokio::time::sleep(d).await;",
            "let i = tokio::time::interval(d);",
            "tokio::time::timeout(d, f).await;",
            "use std::time::{Duration, Instant};",
            "use tokio::time::sleep;",
            "use tokio::time::{sleep, Duration};",
        ]:
            self.write("src/a.rs", f"fn f() {{\n    {line}\n}}\n")
            self.assertEqual(len(guard.problems(self.root)), 1, line)

    def test_a_reading_through_the_clock_passes(self):
        self.write("src/a.rs", "use alondra_core::clock;\nfn f() { clock::instant(); clock::sleep(d); }\n")
        self.assertEqual(guard.problems(self.root), [])

    def test_a_reading_in_the_last_line_of_a_multiline_import_is_named(self):
        self.write("src/a.rs", "use std::time::{\n    Duration,\n    Instant,\n};\n")
        self.assertEqual(len(guard.problems(self.root)), 1)

    def test_test_code_is_outside_the_guard(self):
        self.write("src/a_tests.rs", "fn f() { std::thread::sleep(d); }\n")
        self.write("tests/b.rs", "fn f() { std::thread::sleep(d); }\n")
        self.write("src/c.rs", "fn f() {}\n#[cfg(test)]\nmod tests { fn t() { std::thread::sleep(d); } }\n")
        self.assertEqual(guard.problems(self.root), [])

    def test_a_comment_mentioning_a_call_passes(self):
        self.write("src/a.rs", "// never Instant::now() here\nfn f() {}\n")
        self.assertEqual(guard.problems(self.root), [])


if __name__ == "__main__":
    unittest.main()
