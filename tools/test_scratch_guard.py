#!/usr/bin/env python3
"""A test owns what it creates, in the folder Bazel gives it.

Test-side Rust code never falls back to, or creates anything in, a shared system temporary folder
(/tmp, /private/tmp, /var/tmp): the test wrapper (tools/test-tmpdir.sh) gives each test TMPDIR (Bazel's
TEST_TMPDIR) and, for fixtures that bind a socket, a short private ALONDRA_SHORT_TMPDIR, and removes both
when the test ends. A fixture that cannot find them fails and says so, instead of littering /tmp.
"""
import os
import re
import sys
from pathlib import Path

SKIPPED_DIRS = {".git", ".claude", ".scratch", ".musts", "node_modules", "target", "dist", ".build"}
SHARED = r'"/(?:private/)?(?:var/)?tmp"'
FORBIDDEN = [
    (re.compile(r"canonicalize\(\s*" + SHARED + r"\s*\)"), "resolves a shared temporary folder as a fixture root"),
    (re.compile(r"PathBuf::from\(\s*" + SHARED + r"\s*\)"), "falls back to a shared temporary folder"),
    (re.compile(r"(?:create_dir(?:_all)?|write|tempdir_in|File::create)\(\s*\"/(?:private/)?(?:var/)?tmp[/\"]"), "creates something in a shared temporary folder"),
    (re.compile(r"\benv::temp_dir\b"), "uses the shared temporary folder"),
]
TEST_CRATES = ("crates/scenarios/", "crates/engine-doubles/")


def test_side(relative):
    name = relative.rsplit("/", 1)[-1]
    return "/tests/" in relative or name.endswith("tests.rs") or name.startswith("test_support") or relative.startswith(TEST_CRATES)


def report(root):
    root = Path(root)
    found = []
    for directory, children, files in os.walk(root / "crates"):
        children[:] = [c for c in children if c not in SKIPPED_DIRS and not c.startswith("bazel-")]
        for name in files:
            path = Path(directory) / name
            relative = path.relative_to(root).as_posix()
            if not name.endswith(".rs") or not test_side(relative):
                continue
            for number, line in enumerate(path.read_text().splitlines(), 1):
                for pattern, why in FORBIDDEN:
                    if pattern.search(line):
                        found.append(f"{relative}:{number}: {why}: {line.strip()}")
    return found


if __name__ == "__main__":
    problems = report(sys.argv[1] if len(sys.argv) > 1 else Path(__file__).resolve().parents[1])
    print("\n".join(problems))
    sys.exit(1 if problems else 0)
