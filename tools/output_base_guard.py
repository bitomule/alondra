#!/usr/bin/env python3
"""No file tells anyone to start Bazel with an output base of their own outside this checkout.

An output base is about 20 GB. One started in ~/.cache or /tmp skips the shared output base and the
build queue, fills the boot volume and is never removed. A scratch output base lives in the
checkout's .scratch and is started by `make test-fresh`, which deletes it when it ends.
"""
import os
import re
import sys
from pathlib import Path

SKIPPED_DIRS = {".git", ".claude", ".scratch", ".musts", "node_modules", "target", "dist", ".build", ".bazel-output"}
TEXT_SUFFIXES = {".md", ".sh", ".py", ".rs", ".bzl", ".bazel", ".yml", ".yaml", ".toml", ".bazelrc", ""}
CHECKOUT_OWN = ("$(CURDIR)/.scratch/", "$PWD/.scratch/", "$root/.bazel-output", "$$root/.bazel-output")
SETS_UP_ITS_OWN = {
    "tools/worktree-bazel-setup",
    "tests/native/run.py",
    "deploy/apple/prepare.py",
    "tools/output_base_guard.py",
    "tools/tests/test_output_base_guard.py",
    "tools/output_base_guard_test",
    "tools/worktree_setup_test/setup_test.py",
}
OUTPUT_BASE = re.compile(r"--output_base[= ]\s*[\"']?([^\s\"']*)")


def report(root):
    root = Path(root)
    found = []
    for directory, children, files in os.walk(root):
        children[:] = [c for c in children if c not in SKIPPED_DIRS and not c.startswith("bazel-")]
        for name in files:
            path = Path(directory) / name
            relative = path.relative_to(root).as_posix()
            if relative in SETS_UP_ITS_OWN or (path.suffix not in TEXT_SUFFIXES and name != "Makefile"):
                continue
            try:
                lines = path.read_text().splitlines()
            except (UnicodeDecodeError, OSError):
                continue
            for number, line in enumerate(lines, 1):
                for match in OUTPUT_BASE.finditer(line):
                    if not match.group(1).startswith(CHECKOUT_OWN):
                        found.append(f"{relative}:{number}: starts Bazel with an output base outside this checkout's .scratch: {line.strip()}")
    return found


if __name__ == "__main__":
    problems = report(sys.argv[1] if len(sys.argv) > 1 else Path(__file__).resolve().parents[1])
    print("\n".join(problems))
    sys.exit(1 if problems else 0)
