#!/usr/bin/env python3
"""How fast a task's worktree is where it lives: `make worktree-volume-bench ROOT=<folder>`.

In a throwaway worktree made in ROOT at one commit it times `git worktree add`, a clean build (a new
Bazel output base with the usual caches, as a new task pays), an incremental build after one change,
and the bazel/test targets of MUSTS.yml. During each build it samples, once a second, the CPU and
memory of fseventsd and Spotlight (mds, mds_stores, mdworker) and the disk's transfers. macOS has no
I/O wait figure, so the disk is shown by its transfers per second and MB/s.

Only what it made goes when it ends, also when it fails: its worktrees and their Bazel output base.
"""
import argparse
import json
import os
import re
import secrets
import signal
import statistics
import subprocess
import sys
import threading
import time
from pathlib import Path

BUILD_TARGETS = ["//crates/...", "//apple:AlondraMac"]
CHANGED_FILE = "crates/daemon-support/src/lib.rs"
WORKTREE_ADDS = 5
FSEVENTS = "fseventsd"
SPOTLIGHT = "spotlight"


def elapsed_seconds(output):
    found = re.findall(r"INFO: Elapsed time: ([0-9.]+)s", output)
    return float(found[-1]) if found else None


def test_targets(musts_yml):
    """The targets of every `uses: bazel/test` check, in order, once each."""
    targets, check_uses, in_targets = [], None, False
    for line in musts_yml.splitlines():
        if re.match(r"^  [A-Za-z0-9_-]+:\s*$", line):
            check_uses, in_targets = None, False
        elif uses := re.match(r"^    uses:\s*(\S+)", line):
            check_uses = uses.group(1)
        elif re.match(r"^      targets:\s*$", line):
            in_targets = True
        elif in_targets and (target := re.match(r"^        - (\S+)", line)):
            if check_uses == "bazel/test" and target.group(1) not in targets:
                targets.append(target.group(1))
        elif line.strip():
            in_targets = False
    return targets


def process_group(command):
    name = os.path.basename(command.strip())
    if name == "fseventsd":
        return FSEVENTS
    if name in ("mds", "mds_stores") or name.startswith("mdworker"):
        return SPOTLIGHT
    return None


def ps_sample(listing):
    """CPU % and resident MB of each group in one `ps -A -o %cpu=,rss=,comm=` listing."""
    sample = {FSEVENTS: [0.0, 0.0], SPOTLIGHT: [0.0, 0.0]}
    for line in listing.splitlines():
        parts = line.split(None, 2)
        if len(parts) < 3:
            continue
        group = process_group(parts[2])
        if group:
            sample[group][0] += float(parts[0])
            sample[group][1] += float(parts[1]) / 1024
    return sample


def iostat_disks(line):
    """The disks an `iostat -d` header line names, or None for any other line."""
    names = line.split()
    return names if names and all(name.startswith("disk") for name in names) else None


def iostat_sample(disks, line):
    """Transfers per second and MB/s of each disk in one `iostat -d` line, or None."""
    try:
        values = [float(value) for value in line.split()]
    except ValueError:
        return None
    if len(values) != 3 * len(disks) or not disks:
        return None
    return {disk: (values[3 * index + 1], values[3 * index + 2]) for index, disk in enumerate(disks)}


def spread(values):
    return {"mean": round(statistics.fmean(values), 1), "peak": round(max(values), 1)} if values else None


def summary(ps_samples, io_samples):
    result = {}
    for group in (FSEVENTS, SPOTLIGHT):
        result[group] = {
            "cpu_percent": spread([sample[group][0] for sample in ps_samples]),
            "memory_mb": spread([sample[group][1] for sample in ps_samples]),
        }
    disks = sorted({disk for sample in io_samples for disk in sample})
    result["disks"] = {disk: {
        "transfers_per_second": spread([sample[disk][0] for sample in io_samples if disk in sample]),
        "mb_per_second": spread([sample[disk][1] for sample in io_samples if disk in sample]),
    } for disk in disks}
    return result


class Sampler:
    """Samples once a second from the moment Bazel starts working, not while its call waits in a queue."""

    def __init__(self):
        self.ps, self.io, self.active, self.stop = [], [], threading.Event(), threading.Event()

    def __enter__(self):
        self.iostat = subprocess.Popen(["iostat", "-d", "-w", "1"], stdout=subprocess.PIPE, text=True)
        self.threads = [threading.Thread(target=self._ps, daemon=True), threading.Thread(target=self._iostat, daemon=True)]
        for thread in self.threads:
            thread.start()
        return self

    def _ps(self):
        while not self.stop.wait(1):
            if self.active.is_set():
                listing = subprocess.run(["ps", "-A", "-o", "%cpu=,rss=,comm="], capture_output=True, text=True).stdout
                self.ps.append(ps_sample(listing))

    def _iostat(self):
        since_boot, disks = True, []
        for line in self.iostat.stdout:
            if self.stop.is_set():
                return
            disks = iostat_disks(line) or disks
            sample = iostat_sample(disks, line)
            if sample and since_boot:
                since_boot = False
            elif sample and self.active.is_set():
                self.io.append(sample)

    def __exit__(self, *_):
        self.stop.set()
        self.iostat.terminate()
        self.iostat.wait()

    def result(self):
        return summary(self.ps, self.io)


def bazel(worktree, args, log):
    """Runs Bazel in `worktree`, sampling while it works: its elapsed time and the samples."""
    with Sampler() as sampler, open(log, "a") as out:
        process = subprocess.Popen(["bazelisk", *args], cwd=worktree, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        lines = []
        for line in process.stdout:
            out.write(line)
            lines.append(line)
            if not line.startswith("bazelqueue:"):
                sampler.active.set()
        status = process.wait()
    output = "".join(lines)
    if status != 0:
        raise SystemExit(f"bazelisk {' '.join(args[:2])} failed with status {status}; see {log}")
    return {"seconds": elapsed_seconds(output), **sampler.result()}


def git(repo, *args):
    return subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True, text=True).stdout.strip()


class Worktrees:
    """The worktrees this run makes, each removed with what Bazel kept for it."""

    def __init__(self, repo, root, commit):
        self.repo, self.root, self.commit, self.made, self.built = repo, root, commit, [], set()
        self.tag = secrets.token_hex(4)

    def add(self, name):
        path = self.root / f"worktree-volume-bench-{self.tag}-{name}"
        start = time.perf_counter()
        self.made.append(path)
        git(self.repo, "worktree", "add", "--quiet", "--detach", str(path), self.commit)
        return path, time.perf_counter() - start

    def remove(self, path):
        if path in self.built:
            subprocess.run(["bazelisk", "clean", "--expunge"], cwd=path, capture_output=True)
        if path.exists():
            git(self.repo, "worktree", "remove", "--force", str(path))
        self.made.remove(path)

    def __enter__(self):
        return self

    def __exit__(self, *_):
        for path in list(self.made):
            try:
                self.remove(path)
            except subprocess.CalledProcessError as error:
                print(f"could not remove {path}: {error.stderr}", file=sys.stderr)


def measure(repo, root, commit, log):
    targets = test_targets((repo / "MUSTS.yml").read_text())
    with Worktrees(repo, root, commit) as worktrees:
        adds = []
        for index in range(WORKTREE_ADDS):
            path, seconds = worktrees.add(f"add-{index}")
            adds.append(seconds)
            worktrees.remove(path)
        bench, _ = worktrees.add("build")
        worktrees.built.add(bench)
        clean = bazel(bench, ["build", *BUILD_TARGETS], log)
        with open(bench / CHANGED_FILE, "a") as changed:
            changed.write("\n// worktree-volume-bench\n")
        incremental = bazel(bench, ["build", *BUILD_TARGETS], log)
        tests = bazel(bench, ["test", "--test_output=errors", "--", *targets], log)
    return {
        "root": str(root), "commit": commit, "at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "worktree_add_seconds": {"median": round(statistics.median(adds), 2), "min": round(min(adds), 2), "max": round(max(adds), 2)},
        "clean_build": clean, "incremental_build": incremental, "tests": tests,
    }


def shown(value, unit=""):
    return "–" if value is None else f"{value['mean']}{unit} / {value['peak']}{unit}"


def markdown(result):
    rows = [
        f"Folder: `{result['root']}` · commit `{result['commit'][:12]}` · {result['at']}",
        "",
        f"`git worktree add`: median {result['worktree_add_seconds']['median']} s "
        f"(min {result['worktree_add_seconds']['min']}, max {result['worktree_add_seconds']['max']}, {WORKTREE_ADDS} runs)",
        "",
        "| Run | Bazel time | fseventsd CPU (mean / peak) | fseventsd memory | Spotlight CPU | Spotlight memory |",
        "|---|---|---|---|---|---|",
    ]
    runs = (("Clean build", "clean_build"), ("Incremental build", "incremental_build"), ("Tests", "tests"))
    for name, key in runs:
        run = result[key]
        rows.append(f"| {name} | {run['seconds']} s | {shown(run[FSEVENTS]['cpu_percent'], ' %')} | {shown(run[FSEVENTS]['memory_mb'], ' MB')} | "
                    f"{shown(run[SPOTLIGHT]['cpu_percent'], ' %')} | {shown(run[SPOTLIGHT]['memory_mb'], ' MB')} |")
    rows += ["", "| Run | Disk | Transfers/s (mean / peak) | MB/s (mean / peak) |", "|---|---|---|---|"]
    for name, key in runs:
        for disk, use in result[key]["disks"].items():
            rows.append(f"| {name} | {disk} | {shown(use['transfers_per_second'])} | {shown(use['mb_per_second'])} |")
    return "\n".join(rows)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", required=True, type=Path, help="the folder the throwaway worktrees are made in")
    parser.add_argument("--commit", default="HEAD")
    parser.add_argument("--json", type=Path, help="where to write the numbers as JSON")
    parser.add_argument("--log", type=Path, help="where Bazel's output goes")
    args = parser.parse_args()
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    repo = Path(git(Path.cwd(), "rev-parse", "--show-toplevel"))
    if not args.root.is_dir():
        raise SystemExit(f"{args.root} is not a folder: the measurement never makes one")
    commit = git(repo, "rev-parse", "--verify", f"{args.commit}^{{commit}}")
    log = args.log or Path(os.environ.get("TMPDIR", "/tmp")) / f"worktree-volume-bench-{commit[:12]}.log"
    result = measure(repo, args.root.resolve(), commit, log)
    if args.json:
        args.json.write_text(json.dumps(result, indent=2) + "\n")
    print(markdown(result))


if __name__ == "__main__":
    main()
