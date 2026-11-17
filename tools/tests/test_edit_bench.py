#!/usr/bin/env python3
"""The edit bench finds a file's label, the frequent edits, and what an execution log really ran."""
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

path = Path(__file__).resolve().parents[1] / "edit_bench.py"
spec = importlib.util.spec_from_file_location("edit_bench", path)
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)


class EditBenchTest(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        (self.root / "crates" / "core" / "src").mkdir(parents=True)
        (self.root / "crates" / "core" / "BUILD.bazel").write_text("")
        (self.root / "BUILD.bazel").write_text("")
        for name in ("a.rs", "b.rs"):
            (self.root / "crates" / "core" / "src" / name).write_text("")

    def test_a_file_is_labelled_inside_its_closest_package(self):
        self.assertEqual(bench.label_of(self.root, "crates/core/src/a.rs"), "//crates/core:src/a.rs")
        self.assertEqual(bench.label_of(self.root, "Makefile"), "//:Makefile")

    def test_frequent_edits_count_commits_and_skip_deleted_files(self):
        log = "@@1\ncrates/core/src/a.rs\ncrates/core/src/b.rs\n@@2\ncrates/core/src/b.rs\ncrates/core/src/gone.rs\n"
        self.assertEqual(bench.frequent_edits(log, self.root, 5), ["crates/core/src/b.rs", "crates/core/src/a.rs"])

    def test_only_spawns_outside_every_cache_count_as_run(self):
        spawns = [
            {"mnemonic": "Rustc"},
            {"mnemonic": "Rustc", "cacheHit": True},
            {"mnemonic": "TestRunner", "remoteCacheHit": True},
            {"mnemonic": "TestRunner"},
        ]
        log = "\n".join(json.dumps(spawn, indent=1) for spawn in spawns)
        self.assertEqual(dict(bench.executed(log)), {"Rustc": 1, "TestRunner": 1})

    def test_spawns_a_cache_answered_are_counted(self):
        spawns = [{"mnemonic": "Rustc", "cacheHit": True}, {"mnemonic": "Clippy", "remoteCacheHit": True}, {"mnemonic": "Rustc"}]
        self.assertEqual(bench.from_cache("\n".join(json.dumps(spawn) for spawn in spawns)), 2)

    def test_the_critical_path_comes_from_the_last_invocation(self):
        output = (
            "INFO: Elapsed time: 40.1s, Critical Path: 35.7s\n"
            "INFO: Elapsed time: 228.1s, Critical Path: 220.2s\n"
        )
        self.assertEqual(bench.critical_path(output), 220.2)
        self.assertIsNone(bench.critical_path("no summary"))

    def test_the_time_is_what_bazel_ran_never_the_queue_wait(self):
        output = (
            "bazelqueue: finished 1-2 exit=0 queued=15m34s ran=2m49s failed=unreported\n"
            "bazelqueue: finished 3-4 exit=0 queued=1m ran=850ms failed=unreported\n"
        )
        self.assertAlmostEqual(bench.bazel_seconds(output, 1104), 169.85)
        self.assertEqual(bench.bazel_seconds("INFO: Elapsed time: 3.0s", 42), 42)

    def test_every_run_edits_differently_so_no_cache_serves_it(self):
        self.assertNotEqual(bench.edit_text(".rs", 1), bench.edit_text(".rs", 2))
        self.assertIn("static EDIT_BENCH: u64 = 7;", bench.edit_text(".rs", 7))
        self.assertEqual(bench.edit_text(".py", 7), "# edit-bench 7\n")

    def test_a_retried_invocation_is_told_apart(self):
        self.assertEqual(bench.attempts("INFO: Elapsed time: 1s\nINFO: Elapsed time: 2s\n"), 2)
        self.assertEqual(bench.attempts(""), 1)


if __name__ == "__main__":
    unittest.main()
