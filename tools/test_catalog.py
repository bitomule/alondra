#!/usr/bin/env python3
"""Every Rust test by target, and whether a change lost one.

`bazel run //tools:test_catalog -- list [--targets EXPR] > catalog.tsv` builds the Rust test targets
of EXPR (`//crates/...` unless given, `manual` ones included), asks each test binary for its tests
and prints `target<TAB>test<TAB>run|ignored|manual`, with one line per conversation scenario too.

`bazel run //tools:test_catalog -- compare BEFORE AFTER [--moves FILE]` fails when a test of BEFORE
is in no target of AFTER, or ran and no longer does (`#[ignore]`, or a `manual` target). A test
that only changed target or module is the same test: it is found by its function name. One that
was renamed, merged into another, replaced by a contract case or left to run on request is a line
of FILE, `old test<TAB>where it went<TAB>why`; a line FILE has for a test that neither left nor
stopped fails too, so the table says only what happened.

Under bazelqueue build the script and run it outside Bazel, as `edit_bench` does:
`bazelisk run --script_path=.scratch/test_catalog //tools:test_catalog && .scratch/test_catalog list`.
"""
import argparse
import collections
import os
import subprocess
import sys
from pathlib import Path

BAZEL = os.environ.get("BAZEL", "bazelisk")
EXECUTABLE = "str(target.label) + ' ' + target.files_to_run.executable.path if target.files_to_run.executable else ''"
SCENARIOS = "//crates/scenarios:scenarios_test"
CASES = "crates/scenarios/cases"


def listed(output):
    """The tests of a libtest `--list --format terse` output."""
    return [line[: -len(": test")] for line in output.splitlines() if line.endswith(": test")]


def leaf(test):
    return test.rsplit("::", 1)[-1]


def parse(text):
    """`(target, test, runs)` of every line of a catalog: a test runs unless it is `#[ignore]` or
    its target is `manual`."""
    rows = []
    for line in text.splitlines():
        if line.strip():
            target, test, state = line.split("\t")
            rows.append((target, test, state == "run"))
    return rows


def moves_of(text):
    """`{old test: (where it went, why)}` of a table of moves."""
    moves = {}
    for number, line in enumerate(text.splitlines(), 1):
        if not line.strip() or line.startswith("#"):
            continue
        fields = line.split("\t")
        if len(fields) != 3 or not all(field.strip() for field in fields):
            raise ValueError(f"line {number} of the moves is not `old test<TAB>where it went<TAB>why`")
        moves[fields[0]] = (fields[1], fields[2])
    return moves


def compare(before, after, moves):
    """What became of every test of `before`: kept where it was, found elsewhere by its function
    name, explained by the table of moves, or lost. Also the moves nothing needed. A test compiled
    into two targets is one test: it is kept while a target still has it under the same path."""
    there = set((target, test) for target, test, _ in after)
    paths = collections.Counter(test for _, test, _ in after)
    remaining = collections.Counter(leaf(test) for _, test, _ in after)
    kept, missing = [], []
    for test in set(test for _, test, _ in before) & paths.keys():
        remaining[leaf(test)] -= paths[test]
    for target, test, _ in before:
        if (target, test) in there or test in paths:
            kept.append((target, test))
        else:
            missing.append((target, test))
    moved, explained, lost, used = [], [], [], set()
    for target, test in missing:
        if remaining[leaf(test)] > 0:
            remaining[leaf(test)] -= 1
            moved.append((target, test))
        elif test in moves or leaf(test) in moves:
            name = test if test in moves else leaf(test)
            used.add(name)
            explained.append((target, test, *moves[name]))
        else:
            lost.append((target, test))
    was = set((target, test) for target, test, _ in before)
    added = [(target, test) for target, test, _ in after if (target, test) not in was]
    ran = collections.Counter(leaf(test) for _, test, runs in before if runs)
    ran.subtract(collections.Counter(leaf(test) for _, test, runs in after if runs))
    ran.subtract(collections.Counter(leaf(test) for _, test in lost))
    ran.subtract(collections.Counter(leaf(test) for _, test, _, _ in explained))
    stopped = []
    for name in sorted(name for name, count in ran.items() if count > 0):
        if name in moves:
            used.add(name)
            explained.append(("", name, *moves[name]))
        else:
            stopped.append(name)
    return {
        "kept": kept, "moved": moved, "explained": explained, "lost": lost,
        "added": added, "stopped": stopped, "unused": sorted(set(moves) - used),
    }


def report(result):
    lines = [
        "| Tests | Count |", "|---|---:|",
        f"| In a target under the same path | {len(result['kept'])} |",
        f"| In another target or module, same function | {len(result['moved'])} |",
        f"| Explained by the table of moves | {len(result['explained'])} |",
        f"| New | {len(result['added']) - len(result['moved'])} |",
        f"| Lost | {len(result['lost'])} |",
    ]
    for target, test, where, why in result["explained"]:
        lines.append(f"- `{test}`{f' of `{target}`' if target else ''} went to {where}: {why}")
    for target, test in result["lost"]:
        lines.append(f"- LOST: `{test}` of `{target}` is in no target and in no line of the moves")
    for name in result["stopped"]:
        lines.append(f"- STOPPED: `{name}` ran before and is `#[ignore]` or in a `manual` target now, with no line of the moves")
    for name in result["unused"]:
        lines.append(f"- UNUSED: the moves explain `{name}`, which neither left nor stopped running")
    return "\n".join(lines)


def failed(result):
    return bool(result["lost"] or result["stopped"] or result["unused"])


def run(root, *command):
    result = subprocess.run(command, cwd=root, capture_output=True, text=True)
    if result.returncode != 0:
        raise RuntimeError(f"{' '.join(command)}\n{result.stderr[-2000:]}")
    return result.stdout


def state_of(ignored, manual):
    return "ignored" if ignored else "manual" if manual else "run"


def catalog(root, expression):
    tests = f'kind("rust_test", {expression})'
    manual = set(run(root, BAZEL, "query", f'attr(tags, "manual", {tests})').split())
    run(root, BAZEL, "build", "--", *run(root, BAZEL, "query", tests).split())
    executables = run(root, BAZEL, "cquery", tests, "--output=starlark", f"--starlark:expr={EXECUTABLE}")
    execution_root = Path(run(root, BAZEL, "info", "execution_root").strip())
    rows = []
    for line in sorted(set(filter(None, executables.splitlines()))):
        target, path = line.split(" ", 1)
        target = target.removeprefix("@@").removeprefix("@")
        binary = execution_root / path
        ignored = set(listed(run(root, str(binary), "--list", "--format", "terse", "--ignored")))
        for test in listed(run(root, str(binary), "--list", "--format", "terse")):
            rows.append((target, test, state_of(test in ignored, target in manual)))
    cases = Path(root) / CASES
    if cases.is_dir() and SCENARIOS in run(root, BAZEL, "query", f'kind("sh_test", {expression})').split():
        rows.extend((SCENARIOS, case.name, "run") for case in sorted(cases.iterdir()) if case.is_dir())
    return rows


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)
    listing = commands.add_parser("list")
    listing.add_argument("--targets", default="//crates/...")
    comparing = commands.add_parser("compare")
    comparing.add_argument("before")
    comparing.add_argument("after")
    comparing.add_argument("--moves")
    args = parser.parse_args(argv)
    root = os.environ.get("BUILD_WORKSPACE_DIRECTORY") or os.getcwd()
    if args.command == "list":
        for target, test, state in catalog(root, args.targets):
            print(f"{target}\t{test}\t{state}")
        return 0
    start = Path(os.environ.get("BUILD_WORKING_DIRECTORY") or os.getcwd())
    moves = moves_of((start / args.moves).read_text()) if args.moves else {}
    result = compare(parse((start / args.before).read_text()), parse((start / args.after).read_text()), moves)
    print(report(result))
    return 1 if failed(result) else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
