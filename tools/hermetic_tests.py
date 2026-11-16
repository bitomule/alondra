#!/usr/bin/env python3
"""Every test Bazel caches must be hermetic, so a cached PASSED means the test would pass again.

A test reads only what its BUILD target declares, from its runfiles: never the checkout reached by
resolving a runfiles link back to the source tree. A test that cannot be cached (its tags keep it out
of the cache or the sandbox) says why in uncacheable_tests.tsv.
"""
import os
import re
import sys
from pathlib import Path

ALLOWLIST = "tools/uncacheable_tests.tsv"
UNCACHEABLE_TAGS = {"external", "local", "no-cache", "no-remote", "no-remote-cache", "no-remote-exec"}
SKIPPED_DIRS = {".git", ".claude", ".scratch", ".musts", "node_modules", "target", "dist", ".build"}
RULE = re.compile(r"^\[?\s*(\w+)\(\n(.*?)^\)", re.M | re.S)
NAME = re.compile(r'^\s*name = "([^"]+)"', re.M)
TAGS = re.compile(r"^\s*tags = \[(.*?)\]", re.M | re.S)
ESCAPE = re.compile(r"""(?:makefile|Makefile["')\]]*)\s*\)?\s*\.(?:resolve|canonicalize)\(""")
SOURCE_SUFFIXES = (".py", ".rs", ".sh", ".swift", ".ts", ".mjs", ".cjs")


def _walk(root):
    for directory, children, files in os.walk(root):
        children[:] = [c for c in children if c not in SKIPPED_DIRS and not c.startswith("bazel-")]
        yield directory, children, files


def builds(root):
    for directory, children, files in _walk(root):
        if "BUILD.bazel" in files:
            yield Path(directory) / "BUILD.bazel"


def uncacheable(root):
    """Each test target whose tags keep it out of the cache, as `package:name` -> tags."""
    found = {}
    for build in builds(root):
        package = build.parent.relative_to(root).as_posix().removeprefix(".")
        for match in RULE.finditer(build.read_text()):
            kind, body = match.groups()
            if not kind.endswith("_test"):
                continue
            tags = TAGS.search(body)
            name = NAME.search(body)
            if not tags or not name:
                continue
            kept = sorted(UNCACHEABLE_TAGS & set(re.findall(r'"([^"]+)"', tags.group(1))))
            if kept:
                found[f"//{package}:{name.group(1)}"] = kept
    return found


def allowlist(root):
    entries = {}
    for line in (Path(root) / ALLOWLIST).read_text().splitlines():
        if line.strip() and not line.startswith("#"):
            label, reason = line.split("\t", 1)
            entries[label] = reason.strip()
    return entries


def escapes(root):
    """Sources that resolve a runfiles link back to the checkout, reading files nobody declared."""
    found = []
    for directory, children, files in _walk(root):
        for name in sorted(files):
            path = Path(directory) / name
            if not name.endswith(SOURCE_SUFFIXES) or path.relative_to(root).as_posix() == "tools/hermetic_tests.py":
                continue
            text = path.read_text(errors="replace")
            for match in ESCAPE.finditer(text):
                line = text[: match.start()].count("\n") + 1
                found.append(f"{path.relative_to(root).as_posix()}:{line}")
    return found


def report(root):
    root = Path(root)
    listed = allowlist(root)
    tagged = uncacheable(root)
    problems = []
    for label, tags in sorted(tagged.items()):
        if label not in listed:
            problems.append(f"{label} is tagged {', '.join(tags)}: make it hermetic, or say why in {ALLOWLIST}")
        elif not listed[label]:
            problems.append(f"{label} needs a reason in {ALLOWLIST}")
    for label in sorted(set(listed) - set(tagged)):
        problems.append(f"{label} in {ALLOWLIST} is cacheable now: remove its line")
    for where in escapes(root):
        problems.append(f"{where} resolves a runfiles link back to the checkout: declare what it reads as data")
    return problems


if __name__ == "__main__":
    problems = report(sys.argv[1] if len(sys.argv) > 1 else ".")
    print("\n".join(problems))
    sys.exit(1 if problems else 0)
