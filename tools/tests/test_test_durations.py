#!/usr/bin/env python3
"""Test durations come out of build event files, cached results included."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("test_durations", Path(__file__).resolve().parents[1] / "test_durations.py")
durations = importlib.util.module_from_spec(spec)
spec.loader.exec_module(durations)


def result(label, took, cached=False):
    return {
        "id": {"testResult": {"label": label, "run": 1, "shard": 1, "attempt": 1}},
        "testResult": {"status": "PASSED", "testAttemptDuration": took, "cachedLocally": cached},
    }


def completed(label, timeout):
    return {"id": {"targetCompleted": {"label": label}}, "completed": {"success": True, "testTimeout": timeout}}


class DurationsTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.directory = Path(directory.name)

    def file(self, name, lines):
        path = self.directory / name
        path.write_text("\n".join(line if isinstance(line, str) else json.dumps(line) for line in lines))
        return path

    def test_targets_come_slowest_first_with_their_limit(self):
        path = self.file("a.json", [result("//a:fast", "0.5s"), result("//a:slow", "12.25s"), completed("//a:slow", "60s")])
        self.assertEqual(durations.durations([path]), [(12.25, 60.0, "//a:slow"), (0.5, None, "//a:fast")])

    def test_a_cached_result_counts_the_run_that_produced_it(self):
        path = self.file("a.json", [result("//a:cached", "148.035s", cached=True)])
        self.assertEqual(durations.durations([path])[0][0], 148.035)

    def test_the_slowest_attempt_wins_across_files(self):
        first = self.file("a.json", [result("//a:t", "3s")])
        second = self.file("b.json", [result("//a:t", "9s")])
        self.assertEqual(durations.durations([first, second])[0][0], 9.0)

    def test_a_line_cut_by_a_killed_run_is_skipped(self):
        path = self.file("a.json", [result("//a:t", "1s"), '{"id":{"testRes'])
        self.assertEqual(len(durations.durations([path])), 1)

    def test_the_table_marks_a_target_without_a_known_limit(self):
        self.assertIn("| `//a:t` | 1.0 | ? |", durations.table([(1.0, None, "//a:t")]))


if __name__ == "__main__":
    unittest.main()
