#!/usr/bin/env python3
"""What the worktree volume measurement reads from Bazel, ps, iostat and MUSTS.yml, and that it
removes only the worktrees it made."""
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

module_path = Path(__file__).resolve().parents[1] / "worktree_volume_bench.py"
spec = importlib.util.spec_from_file_location("worktree_volume_bench", module_path)
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)


def git(repo, *args):
    subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True)


class WorktreeVolumeBenchTest(unittest.TestCase):
    def test_bazel_time_is_the_last_elapsed_time_it_prints(self):
        output = "bazelqueue: queued at position 3\nINFO: Elapsed time: 2.5s\nINFO: Elapsed time: 812.304s, Critical Path: 700s\n"
        self.assertEqual(bench.elapsed_seconds(output), 812.304)
        self.assertIsNone(bench.elapsed_seconds("bazelqueue: finished exit=143"))

    def test_the_test_run_takes_the_targets_of_every_bazel_test_check_once(self):
        musts = """version: 1
checks:
  prose:
    uses: agent
    with:
      targets:
        - //not:a_test
  crate-core:
    uses: bazel/test
    paths:
      - "crates/core/**"
    with:
      targets:
        - //crates/core/...
        - //tools:guard_test
  tools:
    uses: bazel/test
    with:
      targets:
        - //tools:guard_test
        - //tools:other_test
    paths:
      - "tools/**"
"""
        self.assertEqual(bench.test_targets(musts), ["//crates/core/...", "//tools:guard_test", "//tools:other_test"])

    def test_fseventsd_and_every_spotlight_process_are_summed_by_group(self):
        listing = """ 12.5  20480 /System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/Metadata.framework/Versions/A/Support/mds
  3.0  10240 /System/Library/Frameworks/CoreServices.framework/Frameworks/Metadata.framework/Versions/A/Support/mdworker_shared
  1.5   2048 /System/Library/Frameworks/CoreServices.framework/Frameworks/Metadata.framework/Versions/A/Support/mds_stores
  7.0   4096 /System/Library/Frameworks/CoreServices.framework/Versions/A/Support/fseventsd
 90.0 999999 /usr/bin/rustc
"""
        self.assertEqual(bench.ps_sample(listing), {bench.FSEVENTS: [7.0, 4.0], bench.SPOTLIGHT: [17.0, 32.0]})

    def test_iostat_lines_are_read_per_disk_and_headers_are_skipped(self):
        disks = bench.iostat_disks("              disk0              disk4 ")
        self.assertEqual(disks, ["disk0", "disk4"])
        self.assertIsNone(bench.iostat_disks("    KB/t  tps  MB/s     KB/t  tps  MB/s "))
        self.assertIsNone(bench.iostat_sample(disks, "    KB/t  tps  MB/s     KB/t  tps  MB/s "))
        self.assertEqual(bench.iostat_sample(disks, "   13.30 4321 56.14     0.00    0  0.00"),
                         {"disk0": (4321.0, 56.14), "disk4": (0.0, 0.0)})
        self.assertIsNone(bench.iostat_sample(disks, "   13.30 4321 56.14"), "a line that does not fit the disks")

    def test_a_summary_gives_the_mean_and_peak_of_each_group_and_disk(self):
        ps = [{bench.FSEVENTS: [2.0, 10.0], bench.SPOTLIGHT: [0.0, 30.0]}, {bench.FSEVENTS: [4.0, 12.0], bench.SPOTLIGHT: [6.0, 30.0]}]
        io = [{"disk0": (100.0, 10.0)}, {"disk0": (300.0, 30.0)}]
        result = bench.summary(ps, io)
        self.assertEqual(result[bench.FSEVENTS]["cpu_percent"], {"mean": 3.0, "peak": 4.0})
        self.assertEqual(result[bench.SPOTLIGHT]["memory_mb"], {"mean": 30.0, "peak": 30.0})
        self.assertEqual(result["disks"]["disk0"]["transfers_per_second"], {"mean": 200.0, "peak": 300.0})
        run = {"seconds": 12.0, **result}
        table = bench.markdown({"root": "/w", "commit": "a" * 40, "at": "t", "worktree_add_seconds": {"median": 1.2, "min": 1.0, "max": 2.0},
                                "clean_build": run, "incremental_build": run, "tests": run})
        self.assertIn("| Clean build | 12.0 s | 3.0 % / 4.0 % | 11.0 MB / 12.0 MB | 3.0 % / 6.0 % | 30.0 MB / 30.0 MB |", table)
        self.assertIn("| Tests | disk0 | 200.0 / 300.0 | 20.0 / 30.0 |", table)

    def test_only_the_worktrees_it_made_go_also_when_it_fails(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get("TEST_TMPDIR")) as scratch:
            repo, root = Path(scratch) / "repo", Path(scratch) / "root"
            root.mkdir()
            git(scratch, "init", "-q", "repo")
            git(repo, "-c", "user.email=test@example.invalid", "-c", "user.name=Test", "commit", "-q", "--allow-empty", "-m", "base")
            theirs = root / "theirs"
            git(repo, "worktree", "add", "-q", "--detach", str(theirs))
            commit = bench.git(repo, "rev-parse", "HEAD")
            with self.assertRaises(RuntimeError):
                with bench.Worktrees(repo, root, commit) as worktrees:
                    kept, seconds = worktrees.add("build")
                    self.assertTrue((kept / ".git").is_file())
                    self.assertGreater(seconds, 0)
                    gone, _ = worktrees.add("add-0")
                    worktrees.remove(gone)
                    self.assertFalse(gone.exists())
                    raise RuntimeError("cut halfway")
            self.assertEqual(sorted(path.name for path in root.iterdir()), ["theirs"])
            listed = bench.git(repo, "worktree", "list", "--porcelain")
            self.assertIn(f"worktree {theirs.resolve()}", listed)
            self.assertNotIn("worktree-volume-bench", listed)


if __name__ == "__main__":
    unittest.main()
