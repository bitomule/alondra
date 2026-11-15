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


if __name__ == "__main__":
    unittest.main()
