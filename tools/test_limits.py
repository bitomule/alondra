#!/usr/bin/env python3
"""No test runs without a limit: every test target declares `size` or `timeout`, and a generous one
(`long`, `eternal`, `large`, `enormous`) is `manual` or listed in test_limits_allowlist.tsv with why.
A macro in a .bzl file declares the limit too, or forwards its caller's with `**kwargs`.

BUILD and .bzl files parse as Python, so this reads the source and needs no Bazel server. The limits only hold
with the flags in `.bazelrc` that are checked here too: `--test_timeout` caps every class, and
`--logging=0` stops a full disk from deadlocking the Bazel server, which no test timeout can end."""
import argparse
import ast
import os
from pathlib import Path

SKIPPED = {".git", ".scratch", ".bazel-output", "node_modules", "target", "dist", ".build"}
GENEROUS_TIMEOUTS = {"long", "eternal"}
GENEROUS_SIZES = {"large", "enormous"}
REQUIRED_FLAGS = ["common --logging=0", "test --test_timeout=60,300,600,900"]
ALLOWLIST = "test_limits_allowlist.tsv"


def build_files(root):
    for directory, children, files in os.walk(root):
        children[:] = [c for c in children if c not in SKIPPED and not c.startswith("bazel-")]
        for name in files:
            if name in ("BUILD.bazel", "BUILD") or name.endswith(".bzl"):
                yield Path(directory) / name


def called_name(node):
    if isinstance(node.func, ast.Attribute):
        return node.func.attr
    return node.func.id if isinstance(node.func, ast.Name) else ""


def keyword(call, name):
    for item in call.keywords:
        if item.arg == name:
            try:
                return ast.literal_eval(item.value)
            except ValueError:
                return "<computed>"
    return None


def test_calls(text):
    for node in ast.walk(ast.parse(text)):
        if isinstance(node, ast.Call) and called_name(node).endswith("_test"):
            yield node


def label(package, call):
    return f"//{package}:{keyword(call, 'name')}"


def forwards_limits(call):
    return any(item.arg is None for item in call.keywords)


def problems_in(package, text, allowed):
    found, generous = [], set()
    for call in test_calls(text):
        if forwards_limits(call):
            continue
        target, size, timeout = label(package, call), keyword(call, "size"), keyword(call, "timeout")
        if size is None and timeout is None:
            found.append(f"{target}: declares neither size nor timeout")
        is_generous = timeout in GENEROUS_TIMEOUTS or (timeout is None and size in GENEROUS_SIZES)
        if is_generous and "manual" not in (keyword(call, "tags") or []):
            generous.add(target)
            if target not in allowed:
                found.append(f"{target}: {timeout or size} needs `manual` or an entry in {ALLOWLIST}")
    return found, generous


def allowlist(path):
    entries = {}
    for line in Path(path).read_text().splitlines():
        if line.strip() and not line.startswith("#"):
            target, _, reason = line.partition("\t")
            entries[target] = reason.strip()
    return entries


def report(root):
    root = Path(root)
    allowed = allowlist(root / "tools" / ALLOWLIST)
    problems = [f"{target}: entry in {ALLOWLIST} has no reason" for target, reason in allowed.items() if not reason]
    used = set()
    for path in sorted(build_files(root)):
        package = path.parent.relative_to(root).as_posix().removeprefix(".")
        found, generous = problems_in(package, path.read_text(), allowed)
        problems += found
        used |= generous
    problems += [f"{target}: entry in {ALLOWLIST} is no longer needed" for target in sorted(set(allowed) - used)]
    rc = (root / ".bazelrc").read_text().splitlines()
    problems += [f".bazelrc: `{flag}` is missing" for flag in REQUIRED_FLAGS if flag not in rc]
    return problems


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", nargs="?", default=".")
    problems = report(parser.parse_args().root)
    print("\n".join(problems) or "Every test declares a limit")
    raise SystemExit(1 if problems else 0)


if __name__ == "__main__":
    main()
