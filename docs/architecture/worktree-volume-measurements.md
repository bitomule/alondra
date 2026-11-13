# Worktree volume measurements

How fast a task's worktree is depends on where it lives. These numbers compare the system disk,
where worktrees live today (`<repo>/.claude/worktrees`), with a volume of their own, on the same Mac
and at the same commit.

## How to measure

```sh
make worktree-volume-bench ROOT=<folder> COMMIT=<sha> BENCH_JSON=<file>
```

`tools/worktree_volume_bench.py` makes throwaway worktrees in `ROOT`, with no branch, at `COMMIT`,
and measures in them:

- **`git worktree add`**: five worktrees made one after another; median, min and max.
- **Clean build**: `bazelisk build //crates/... //apple:AlondraMac` in a new worktree. A new path gets
  a new Bazel output base, with the usual disk and remote caches: what a new task pays.
- **Incremental build**: the same build after a change to `crates/daemon-support/src/lib.rs` that no
  cache has seen, so every run compiles it and what depends on it.
- **Tests**: every `bazel/test` target of that commit's `MUSTS.yml`, with `--nocache_test_results`.
- **fseventsd and Spotlight**: once a second during each run, the CPU and resident memory of
  `fseventsd` and of Spotlight (`mds`, `mds_stores`, `mdworker*`), summed per group, as mean and peak.
  CPU is a share of one core, so a peak above 100 % uses more than one.
- **Disk**: transfers per second and MB/s of each disk, from `iostat`. macOS has no I/O wait figure
  (`top` and `iostat` report none), so the disk shows up as how busy it is.

Times are Bazel's own (`Elapsed time`), not the wait in the shared build queue. Sampling starts when
Bazel starts working. The script needs no sudo and removes only what it made, also when it fails or
is stopped: its worktrees and their Bazel output base.

## Reading the numbers

- The Mac is shared with other builds, and the clean build mostly fetches from the caches, so it
  varies from run to run: two runs at the same commit on the system disk took 128.6 s and 40.8 s.
  Compare the incremental build and the tests first, and the clean build only by a wide margin.
- `disk0` is the internal SSD, which holds the worktree on the system disk and the new volume alike.
  `disk4` is an external SSD that holds Bazel's output base on the measured Mac, so most build
  outputs are written there wherever the worktree is.

## Baseline: system disk

Apple M3 Pro, 18 GB, macOS 26.6.2. Folder `<repo>/.claude/worktrees` on the system disk, commit
`e7680bc54ad2`, 13 November 2026.

`git worktree add`: median 4.04 s (min 3.11, max 7.89, 5 runs).

| Run | Bazel time | fseventsd CPU (mean / peak) | fseventsd memory | Spotlight CPU | Spotlight memory |
|---|---|---|---|---|---|
| Clean build | 40.8 s | 8.2 % / 33.6 % | 7.8 MB / 10.8 MB | 14.2 % / 39.9 % | 86.9 MB / 130.2 MB |
| Incremental build | 288.7 s | 1.3 % / 26.4 % | 5.4 MB / 13.4 MB | 14.5 % / 375.4 % | 112.2 MB / 544.0 MB |
| Tests | 502.5 s | 2.9 % / 37.4 % | 7.6 MB / 11.2 MB | 18.2 % / 413.5 % | 317.4 MB / 674.4 MB |

| Run | Disk | Transfers/s (mean / peak) | MB/s (mean / peak) |
|---|---|---|---|
| Clean build | disk0 (internal) | 8640.6 / 21201.0 | 174.6 / 821.5 |
| Clean build | disk4 (external) | 2985.9 / 23798.0 | 140.0 / 730.9 |
| Incremental build | disk0 (internal) | 7131.2 / 23737.0 | 162.9 / 740.8 |
| Incremental build | disk4 (external) | 703.8 / 9696.0 | 25.7 / 306.0 |
| Tests | disk0 (internal) | 12425.2 / 39082.0 | 249.6 / 936.7 |
| Tests | disk4 (external) | 1228.6 / 26279.0 | 17.7 / 276.1 |

Spotlight is the busiest of the two during builds and tests: up to four cores and 674 MB at its
peak, while fseventsd stays under half a core and 14 MB.
