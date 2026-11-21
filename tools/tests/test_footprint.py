#!/usr/bin/env python3
"""The footprint report reads memory sizes, splits hosts by owner and files events by where they happen."""
import importlib.machinery
import importlib.util
from pathlib import Path
import sys
import unittest

path = Path(__file__).resolve().parents[1] / "footprint.py"
loader = importlib.machinery.SourceFileLoader("footprint", str(path))
spec = importlib.util.spec_from_loader("footprint", loader)
footprint = importlib.util.module_from_spec(spec)
sys.modules["footprint"] = footprint
loader.exec_module(footprint)

HOME = "/Users/ana"
ALONDRA = "/Users/ana/.alondra"


class Memory(unittest.TestCase):
    def test_top_sizes_become_megabytes(self):
        self.assertEqual(footprint.megabytes("6064K"), 6064 / 1024)
        self.assertEqual(footprint.megabytes("98M"), 98.0)
        self.assertEqual(footprint.megabytes("1.5G"), 1536.0)
        self.assertEqual(footprint.megabytes("120M+"), 120.0)
        self.assertEqual(footprint.megabytes("n/a"), 0.0)


class Hosts(unittest.TestCase):
    def test_a_host_whose_parent_is_gone_counts_as_orphaned(self):
        table = [
            footprint.Process(10, 1, 0, "/opt/homebrew/Cellar/alondra/0.3.1/libexec/alondra/sdk/alondra-host x"),
            footprint.Process(11, 500, 0, "/opt/homebrew/Cellar/alondra/0.3.2/libexec/alondra/sdk/alondra-host x"),
            footprint.Process(12, 500, 0, "/opt/homebrew/Cellar/alondra/0.3.2/libexec/alondra/codex/alondra-codex-host"),
            footprint.Process(13, 1, 0, "/usr/bin/some-alondra-host-helper"),
        ]
        split = footprint.hosts(table, {10: 50.0, 11: 90.0, 12: 10.0, 13: 999.0})
        self.assertEqual(split["orphaned"], (1, 50.0))
        self.assertEqual(split["owned"], (2, 100.0))


class Events(unittest.TestCase):
    def test_each_path_goes_to_where_it_happens(self):
        cases = {
            "/Volumes/Disk/bazel/0123abcd/execroot/_main/bazel-out/x.o": "Bazel output bases",
            "/Volumes/Disk/bazel/0123abcd/rustc-incremental/crate-1/s-abc/dep-graph.bin": "Bazel output bases",
            "/Volumes/Cache/disk-cache/cas/ab/abcdef": "Bazel caches",
            "/Volumes/Disk/.Spotlight-V100/Store-V2/x": "Spotlight index",
            "/private/tmp/bt.Ab12Cd/at-xyz/work/home/admission.json": "test temporary folders",
            "/System/Volumes/Data/Users/ana/Projects/app/.claude/worktrees/t-one/src/main.rs": "task worktrees",
            "/Users/ana/.alondra/domain.db-wal": "Alondra home",
            "/private/var/folders/xy/T/scratch": "TMPDIR",
            "/private/tmp/other": "/tmp other",
            "/Users/ana/Documents/note.txt": "home other",
            "/Library/Caches/x": "other",
        }
        for event, expected in cases.items():
            with self.subTest(event=event):
                self.assertEqual(footprint.category(event, HOME, ALONDRA), expected)

    def test_a_path_that_changes_twice_is_one_distinct_path(self):
        counts = footprint.tally([
            "/Users/ana/.alondra/domain.db-wal",
            "/Users/ana/.alondra/domain.db-wal",
            "/Users/ana/.alondra/domain.db-shm",
            "/private/tmp/bt.Ab12Cd/a",
        ], HOME, ALONDRA)
        self.assertEqual(counts, {"Alondra home": (3, 2), "test temporary folders": (1, 1)})


if __name__ == "__main__":
    unittest.main()
