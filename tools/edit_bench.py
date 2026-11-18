#!/usr/bin/env python3
"""What one-line edits cost: the crates and test targets each reopens, and what Bazel really reruns.

`bazel run //tools:edit_bench -- [--edit PATH ...] [--tracked] [--build-only] [--bazel-flag FLAG ...]
[--query-only]`. Without --edit it measures the source files edited most often in the last two
weeks; --tracked measures the edits the build graph's targets are set for. For each edit it makes a
first one-line change and builds the affected targets, so rustc's incremental state exists for every
crate the edit reaches, as it does in a real edit loop (a crate taken from a cache has none). Then it
makes a second change that reaches the binaries (a `#[used]` static in Rust, a comment elsewhere)
and times `bazel build` over the same targets: what compiling costs. A third change times `bazel
test` with an execution log: the whole edit, and the test target that took longest. It puts the
file back exactly as it was, also when interrupted. `--build-only` stops after the build.

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
TRACKED = [
    "crates/views/src/task_state.rs",
    "crates/engine-admission/src/admission.rs",
    "crates/engine-orchestrator/src/domain_orchestrator.rs",
    "crates/engine-live/src/node_reports.rs",
    "crates/domain-rules/src/rules.rs",
    "crates/github-gh/src/cli.rs",
    "crates/engine-sdk/src/sdk_owner.rs",
    "crates/alondrad/src/assemble.rs",
]
NOT_PASSED = re.compile(r"^(//\S+)\s+(?:FAILED|TIMEOUT|NO STATUS)\b", re.M)
RAN_TEST = re.compile(r"^(//\S+)\s+(?:PASSED|FAILED|FLAKY|TIMEOUT)\b.* in (\d+(?:\.\d+)?)s", re.M)
CRITICAL = re.compile(r"Critical Path: (\d+(?:\.\d+)?)s")
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


def spawns(log_text):
    decoder = json.JSONDecoder()
    index = 0
    while index < len(log_text):
        while index < len(log_text) and log_text[index].isspace():
            index += 1
        if index >= len(log_text):
            break
        spawn, index = decoder.raw_decode(log_text, index)
        yield spawn


def cached(spawn):
    return bool(spawn.get("cacheHit") or spawn.get("remoteCacheHit"))


def executed(log_text):
    """Spawns the execution log ran instead of taking from a cache, counted by mnemonic."""
    return collections.Counter(spawn.get("mnemonic", "?") for spawn in spawns(log_text) if not cached(spawn))


def from_cache(log_text):
    """Spawns the disk or remote cache answered."""
    return sum(1 for spawn in spawns(log_text) if cached(spawn))


def critical_path(output):
    """Bazel's critical path of the last invocation in the output, in seconds."""
    found = CRITICAL.findall(output)
    return float(found[-1]) if found else None


def slowest_test(output):
    """The test target of Bazel's summary that took longest among those it ran, with its seconds;
    a result read from a cache did not run."""
    ran = [(float(seconds), label) for label, seconds in RAN_TEST.findall(output)]
    return max(ran)[::-1] if ran else None


def not_passed(output):
    """The test targets of Bazel's summary that failed, timed out or never ran."""
    return sorted(set(NOT_PASSED.findall(output)))


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


def bazel(root, verb, targets, *extra):
    started = time.monotonic()
    result = subprocess.run(
        [BAZEL, verb, "--test_output=errors", *extra, "--", *targets],
        cwd=root, capture_output=True, text=True,
    )
    output = result.stdout + result.stderr
    return result.returncode, bazel_seconds(output, time.monotonic() - started), attempts(output), output


def measure(root, relative, query_only, build_only=False, flags=()):
    label = label_of(root, relative)
    crates, tests = reopened(root, label)
    row = {"edit": relative, "crates": len(crates), "tests": len(tests)}
    if query_only or not tests:
        return row
    verb = "build" if build_only else "test"
    path = Path(root) / relative
    original = path.read_bytes()
    with tempfile.TemporaryDirectory() as directory:
        log = Path(directory) / "execution.json"
        try:
            path.write_bytes(original + edit_text(path.suffix, time.time_ns()).encode())
            bazel(root, "build", tests, *flags)
            if not build_only:
                path.write_bytes(original + edit_text(path.suffix, time.time_ns()).encode())
                _, build_seconds, _, build_output = bazel(root, "build", tests, *flags)
                row.update(build_seconds=round(build_seconds), build_critical=critical_path(build_output))
            path.write_bytes(original + edit_text(path.suffix, time.time_ns()).encode())
            code, seconds, tries, output = bazel(root, verb, tests, *flags, f"--execution_log_json_file={log}")
        finally:
            path.write_bytes(original)
        text = log.read_text() if log.exists() else ""
        ran = executed(text)
    row.update(
        verb=verb,
        seconds=round(seconds),
        critical=critical_path(output),
        slowest=slowest_test(output),
        failed=not_passed(output),
        cached=from_cache(text),
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
        "| Edit | Crates reopened | Test targets reopened | Actions run (rustc / clippy / tests) | From cache"
        " | Build: critical path | Edit + build | Longest test target | Critical path | Edit + build or test |",
        "|---|---:|---:|---|---:|---:|---:|---|---:|---:|",
    ]
    for row in rows:
        if "seconds" in row:
            ran = f"{row['actions']} ({row['rustc']} / {row['clippy']} / {row['test_runs']})"
            failed = ": " + ", ".join(f"`{label}`" for label in row["failed"]) if row.get("failed") else ""
            took = f"{row['seconds']} s ({row['verb']})" + ("" if row["passed"] else f" (failed{failed})")
            took += " (retried: actions count only the last attempt)" if row["retried"] else ""
            critical = "-" if row["critical"] is None else f"{round(row['critical'])} s"
            cache = str(row["cached"])
            build_critical = "-" if row.get("build_critical") is None else f"{round(row['build_critical'])} s"
            build = f"{row['build_seconds']} s" if "build_seconds" in row else "-"
            slowest = f"`{row['slowest'][0]}` {round(row['slowest'][1])} s" if row.get("slowest") else "-"
        else:
            ran = took = critical = cache = build_critical = build = slowest = "-"
        lines.append(
            f"| `{row['edit']}` | {row['crates']} | {row['tests']} | {ran} | {cache} | {build_critical} | {build}"
            f" | {slowest} | {critical} | {took} |"
        )
    return "\n".join(lines)


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--edit", action="append", default=[], help="a source file, relative to the checkout")
    parser.add_argument("--count", type=int, default=12, help="how many frequent edits without --edit")
    parser.add_argument("--since", default="14.days", help="the git history the frequent edits come from")
    parser.add_argument("--query-only", action="store_true", help="count what each edit reopens; run nothing")
    parser.add_argument("--tracked", action="store_true", help="measure the edits the build graph's targets are set for")
    parser.add_argument("--build-only", action="store_true", help="time `bazel build` of the targets instead of `bazel test`")
    parser.add_argument("--bazel-flag", action="append", default=[], help="a flag every Bazel invocation gets, as --bazel-flag=--compilation_mode=opt")
    args = parser.parse_args(argv)
    root = os.environ.get("BUILD_WORKSPACE_DIRECTORY") or os.getcwd()
    edits = args.edit + (TRACKED if args.tracked else [])
    if not edits:
        log = subprocess.run(
            ["git", "log", f"--since={args.since}", "--name-only", "--format=@@%h", "--", "crates"],
            cwd=root, capture_output=True, text=True, check=True,
        ).stdout
        edits = frequent_edits(log, root, args.count)
    rows = [measure(root, edit, args.query_only, args.build_only, args.bazel_flag) for edit in edits]
    print(table(rows))
    return 0 if all(row.get("passed", True) for row in rows) else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
