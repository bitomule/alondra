#!/usr/bin/env python3
"""The edit bench finds a file's label, the frequent edits, and what an execution log really ran."""
import contextlib
import gzip
import importlib.util
import io
import json
import tempfile
import unittest
from unittest import mock
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
        source = "fn a() {\n}\n"
        self.assertNotEqual(bench.edited(source, ".rs", 1), bench.edited(source, ".rs", 2))
        self.assertEqual(bench.edited("x = 1\n", ".py", 7), "x = 1\n# edit-bench 7\n")

    def test_a_rust_edit_opens_the_body_of_the_first_function(self):
        source = (
            "use std::fmt;\n\nconst fn fixed() -> u8 {\n    1\n}\n\nfn split(\n    a: u8,\n) -> u8 {\n    a\n}\n\n"
            "    pub(crate) async fn turns(queue: &[u8]) -> Vec<u8> {\n        queue.to_vec()\n    }\n"
        )
        lines = bench.edited(source, ".rs", 7).split("\n")
        opened = lines.index("    pub(crate) async fn turns(queue: &[u8]) -> Vec<u8> {")
        self.assertEqual(lines[opened + 1], "        std::hint::black_box(7u64);")
        self.assertEqual(len(lines), len(source.split("\n")) + 1, "one line added, nothing else")

    def test_a_rust_file_without_a_function_gets_a_static(self):
        self.assertEqual(bench.edited("const A: u8 = 1;\n", ".rs", 7), "const A: u8 = 1;\n\n#[used]\nstatic EDIT_BENCH: u64 = 7;\n")

    def test_the_critical_path_lists_its_actions_in_order_without_bookkeeping(self):
        events = [
            {"cat": "critical path component", "ph": "X", "ts": 30, "dur": 74_000_000, "name": "action 'Compiling Rust rlib daemon_server v0.3.69 (52 files)'"},
            {"cat": "critical path component", "ph": "X", "ts": 10, "dur": 5_500_000, "name": "action 'Compiling Rust rlib engine_admission v0.3.69 (8 files)'"},
            {"cat": "critical path component", "ph": "X", "ts": 20, "dur": 100_000, "name": "runfiles for //crates/scenarios:scenarios_test"},
            {"cat": "action processing", "ph": "X", "ts": 40, "dur": 90_000_000, "name": "Compiling Rust rlib cli_mcp"},
        ]
        profile = self.root / "build.profile.gz"
        with gzip.open(profile, "wt") as file:
            json.dump({"traceEvents": events}, file)
        self.assertEqual(bench.critical_components(profile), [
            ("Compiling Rust rlib engine_admission v0.3.69 (8 files)", 5.5),
            ("Compiling Rust rlib daemon_server v0.3.69 (52 files)", 74.0),
        ])

    def test_the_longest_test_is_one_bazel_ran_never_a_cached_result(self):
        output = (
            "//crates/alondrad:alondrad_test                                  (cached) PASSED in 300.0s\n"
            "//crates/scenarios:scenarios_test                                        PASSED in 144.0s\n"
            "//crates/alondrad:relaunch_test                                          FAILED in 34.2s\n"
            "//crates/engine-sdk:engine_sdk_test                                     TIMEOUT in 600.0s\n"
        )
        self.assertEqual(bench.slowest_test(output), ("//crates/engine-sdk:engine_sdk_test", 600.0))
        self.assertIsNone(bench.slowest_test("//crates/core:alondra_core_test (cached) PASSED in 44.5s\n"))

    def test_a_failed_run_names_the_targets_that_did_not_pass(self):
        output = (
            "//crates/alondrad:relaunch_test                                          FAILED in 34.2s\n"
            "//crates/engine-sdk:engine_sdk_test                                     TIMEOUT in 600.0s\n"
            "//crates/scenarios:scenarios_test                                        PASSED in 60.0s\n"
            "//crates/views:views_test                                             NO STATUS\n"
        )
        failed = ["//crates/alondrad:relaunch_test", "//crates/engine-sdk:engine_sdk_test", "//crates/views:views_test"]
        self.assertEqual(bench.not_passed(output), failed)
        row = {
            "edit": "a.rs", "crates": 1, "tests": 2, "verb": "test", "seconds": 9, "critical": 8.0, "cached": 0,
            "passed": False, "retried": False, "rustc": 1, "clippy": 1, "test_runs": 2, "actions": 4,
            "failed": failed[:1],
        }
        self.assertIn("9 s (test) (failed: `//crates/alondrad:relaunch_test`) |", bench.table([row]))

    def test_the_logs_of_failed_targets_outlive_the_next_pass_and_only_the_newest_runs_stay(self):
        logs = self.root / ".bazel-links" / "bazel-testlogs" / "crates" / "alondrad" / "relaunch_test"
        (logs / "test.attempts").mkdir(parents=True)
        (logs / "test.log").write_text("daemon exited signal: 9 (SIGKILL)")
        (logs / "test.attempts" / "attempt_1.log").write_text("first attempt")
        failed = ["//crates/alondrad:relaunch_test", "//crates/views:views_test"]
        kept = bench.keep_failures(self.root, "crates/views/src/task_state.rs", failed)
        self.assertTrue(kept.name.endswith("-crates_views_src_task_state.rs"), kept)
        self.assertEqual((kept / "crates/alondrad/relaunch_test/test.log").read_text(), "daemon exited signal: 9 (SIGKILL)")
        self.assertEqual((kept / "crates/alondrad/relaunch_test/test.attempts/attempt_1.log").read_text(), "first attempt")
        self.assertFalse((kept / "crates/views").exists(), "a target with no log leaves nothing")
        for _ in range(bench.KEPT_RUNS):
            bench.keep_failures(self.root, "a.rs", failed[:1])
        runs = sorted((self.root / bench.FAILURES).iterdir())
        self.assertEqual(len(runs), bench.KEPT_RUNS)
        self.assertNotIn(kept, runs, "the oldest run goes first")
        self.assertIsNone(bench.keep_failures(self.root, "a.rs", failed[1:]), "nothing to keep, no folder")

    def test_a_sharded_target_keeps_the_logs_of_every_shard(self):
        logs = self.root / ".bazel-links" / "bazel-testlogs" / "crates" / "scenarios" / "scenarios_test"
        for shard in ("shard_1_of_2", "shard_2_of_2"):
            (logs / shard).mkdir(parents=True)
            (logs / shard / "test.log").write_text(shard)
        (logs / "shard_2_of_2" / "test.attempts").mkdir()
        (logs / "shard_2_of_2" / "test.attempts" / "attempt_1.log").write_text("first attempt")
        kept = bench.keep_failures(self.root, "a.rs", ["//crates/scenarios:scenarios_test"])
        self.assertIsNotNone(kept, "a sharded target has logs to keep")
        copied = kept / "crates/scenarios/scenarios_test"
        self.assertEqual((copied / "shard_1_of_2/test.log").read_text(), "shard_1_of_2")
        self.assertEqual((copied / "shard_2_of_2/test.log").read_text(), "shard_2_of_2")
        self.assertEqual((copied / "shard_2_of_2/test.attempts/attempt_1.log").read_text(), "first attempt")

    def test_a_bench_cut_halfway_has_printed_the_rows_it_measured(self):
        def measure(root, edit, *_):
            if edit == "b.rs":
                raise KeyboardInterrupt
            return {"edit": edit, "crates": 1, "tests": 2}

        output = io.StringIO()
        with mock.patch.object(bench, "measure", measure), contextlib.redirect_stdout(output):
            with self.assertRaises(KeyboardInterrupt):
                bench.main(["--edit", "a.rs", "--edit", "b.rs"])
        self.assertEqual(output.getvalue().splitlines()[-1], "| `a.rs` | 1 | 2 | - | - | - | - | - | - | - |")

    def test_a_row_tells_compiling_from_testing(self):
        row = {
            "edit": "crates/github-gh/src/cli.rs", "crates": 10, "tests": 36, "verb": "test", "seconds": 150,
            "critical": 146.2, "cached": 3, "passed": True, "retried": False, "rustc": 11, "clippy": 5,
            "test_runs": 39, "actions": 56, "build_seconds": 41, "build_critical": 29.6,
            "slowest": ("//crates/scenarios:scenarios_test", 143.8),
        }
        line = bench.table([row]).splitlines()[-1]
        self.assertIn("| 30 s | 41 s | `//crates/scenarios:scenarios_test` 144 s | 146 s | 150 s (test) |", line)
        counted = bench.table([{"edit": "a.rs", "crates": 1, "tests": 2}]).splitlines()[-1]
        self.assertEqual(counted, "| `a.rs` | 1 | 2 | - | - | - | - | - | - | - |")

    def test_the_tracked_edits_cover_an_adapter_the_engine_the_root_and_the_domain(self):
        for edit in ("crates/github-gh/src/cli.rs", "crates/engine-sdk/src/sdk_owner.rs",
                     "crates/alondrad/src/assemble.rs", "crates/domain-rules/src/rules.rs"):
            self.assertIn(edit, bench.TRACKED)

    def test_a_retried_invocation_is_told_apart(self):
        self.assertEqual(bench.attempts("INFO: Elapsed time: 1s\nINFO: Elapsed time: 2s\n"), 2)
        self.assertEqual(bench.attempts(""), 1)


if __name__ == "__main__":
    unittest.main()
