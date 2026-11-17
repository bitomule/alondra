#!/usr/bin/env python3
"""What one-line edits cost: the crates and test targets each reopens, and what Bazel really reruns.

`bazel run //tools:edit_bench -- [--edit PATH ...] [--query-only]`. Without --edit it measures the
source files edited most often in the last two weeks. For each edit it tests the affected targets
once so the cache is warm, makes a one-line change that reaches the binaries (a `#[used]` static in
Rust, a comment elsewhere), times `bazel test` over the same targets with an execution log, and puts
the file back exactly as it was, also when interrupted.

Under bazelqueue a `bazel run` holds its build slot while the bench runs, so on a shared Mac build
the script and run it outside Bazel:
`bazel run --script_path=.scratch/edit_bench //tools:edit_bench && .scratch/edit_bench [args]`.
The time reported is what Bazel ran, from bazelqueue's summary when there is one, never the wait.
"""
import argparse
import collections
import json
import os
import re
import subprocess
import sys
import tempfile
import time
from pathlib import Path

BAZEL = os.environ.get("BAZEL", "bazelisk")
LIBRARIES = "rust_library|rust_binary|rust_proc_macro"
COMMENT = {".rs": "//", ".py": "#", ".swift": "//", ".ts": "//"}
QUEUE_RAN = re.compile(r"^bazelqueue: finished \S+ exit=\d+ queued=\S+ ran=(\S+)", re.M)
DURATION = re.compile(r"(\d+(?:\.\d+)?)(h|ms|m|s)")


def label_of(root, relative):
    """The Bazel label of a source file: its path inside the closest package above it."""
    path = Path(relative)
    for parent in path.parents:
        if (Path(root) / parent / "BUILD.bazel").exists():
            package = "" if parent == Path(".") else parent.as_posix()
            return f"//{package}:{path.relative_to(parent).as_posix()}"
    raise ValueError(f"{relative} is in no Bazel package")


def frequent_edits(log, root, count):
    """The source files that appear in most commits of a `git log --name-only --format=@@` output."""
    commits = collections.Counter()
    for line in log.splitlines():
        if line and not line.startswith("@@") and Path(line).suffix in COMMENT and (Path(root) / line).exists():
            commits[line] += 1
    return [path for path, _ in commits.most_common(count)]


def executed(log_text):
    """Spawns the execution log ran instead of taking from a cache, counted by mnemonic."""
    decoder = json.JSONDecoder()
    found = collections.Counter()
    index = 0
    while index < len(log_text):
        while index < len(log_text) and log_text[index].isspace():
            index += 1
        if index >= len(log_text):
            break
        spawn, index = decoder.raw_decode(log_text, index)
        if not spawn.get("cacheHit") and not spawn.get("remoteCacheHit"):
            found[spawn.get("mnemonic", "?")] += 1
    return found


def seconds_of(text):
    """`1m03s`, `25m53s`, `11.7s` or `850ms` in seconds."""
    scale = {"h": 3600, "m": 60, "s": 1, "ms": 0.001}
    return sum(float(number) * scale[unit] for number, unit in DURATION.findall(text))


def bazel_seconds(output, wall):
    """What Bazel ran according to bazelqueue's summaries, without the queue's wait; else the wall time."""
    ran = QUEUE_RAN.findall(output)
    return sum(seconds_of(value) for value in ran) if ran else wall


def edit_text(suffix, nonce):
    """A one-line change unique to this run: the same text twice would be served from the cache of
    the run before, and the bench would measure nothing."""
    if suffix == ".rs":
        return f"\n#[used]\nstatic EDIT_BENCH: u64 = {nonce};\n"
    return f"{COMMENT[suffix]} edit-bench {nonce}\n"


def attempts(output):
    return max(1, output.count("INFO: Elapsed time:"))


def query(root, expression):
    result = subprocess.run([BAZEL, "query", expression], cwd=root, capture_output=True, text=True)
    if result.returncode != 0:
        raise RuntimeError(result.stderr[-2000:])
    return [line for line in result.stdout.splitlines() if line]


def reopened(root, label):
    crates = query(root, f'kind("{LIBRARIES}", rdeps(//..., {label}))')
    tests = query(root, f'kind(".*_test", rdeps(//..., {label})) except attr(tags, "manual", //...)')
    return crates, tests


def bazel_test(root, targets, *extra):
    started = time.monotonic()
    result = subprocess.run(
        [BAZEL, "test", "--test_output=errors", *extra, "--", *targets],
        cwd=root, capture_output=True, text=True,
    )
    output = result.stdout + result.stderr
    return result.returncode, bazel_seconds(output, time.monotonic() - started), attempts(output)


def measure(root, relative, query_only):
    label = label_of(root, relative)
    crates, tests = reopened(root, label)
    row = {"edit": relative, "crates": len(crates), "tests": len(tests)}
    if query_only or not tests:
        return row
    bazel_test(root, tests)
    path = Path(root) / relative
    original = path.read_bytes()
    with tempfile.TemporaryDirectory() as directory:
        log = Path(directory) / "execution.json"
        try:
            path.write_bytes(original + edit_text(path.suffix, time.time_ns()).encode())
            code, seconds, tries = bazel_test(root, tests, f"--execution_log_json_file={log}")
        finally:
            path.write_bytes(original)
        ran = executed(log.read_text()) if log.exists() else collections.Counter()
    row.update(
        seconds=round(seconds),
        passed=code == 0,
        retried=tries > 1,
        rustc=ran.get("Rustc", 0),
        clippy=ran.get("Clippy", 0),
        test_runs=ran.get("TestRunner", 0),
        actions=sum(ran.values()),
    )
    return row


def table(rows):
    lines = [
        "| Edit | Crates reopened | Test targets reopened | Actions run (rustc / clippy / tests) | Edit + test |",
        "|---|---:|---:|---|---:|",
    ]
    for row in rows:
        if "seconds" in row:
            ran = f"{row['actions']} ({row['rustc']} / {row['clippy']} / {row['test_runs']})"
            took = f"{row['seconds']} s" + ("" if row["passed"] else " (failed)")
            took += " (retried: actions count only the last attempt)" if row["retried"] else ""
        else:
            ran = took = "-"
        lines.append(f"| `{row['edit']}` | {row['crates']} | {row['tests']} | {ran} | {took} |")
    return "\n".join(lines)


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--edit", action="append", default=[], help="a source file, relative to the checkout")
    parser.add_argument("--count", type=int, default=12, help="how many frequent edits without --edit")
    parser.add_argument("--since", default="14.days", help="the git history the frequent edits come from")
    parser.add_argument("--query-only", action="store_true", help="count what each edit reopens; run nothing")
    args = parser.parse_args(argv)
    root = os.environ.get("BUILD_WORKSPACE_DIRECTORY") or os.getcwd()
    edits = args.edit
    if not edits:
        log = subprocess.run(
            ["git", "log", f"--since={args.since}", "--name-only", "--format=@@%h", "--", "crates"],
            cwd=root, capture_output=True, text=True, check=True,
        ).stdout
        edits = frequent_edits(log, root, args.count)
    rows = [measure(root, edit, args.query_only) for edit in edits]
    print(table(rows))
    return 0 if all(row.get("passed", True) for row in rows) else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
