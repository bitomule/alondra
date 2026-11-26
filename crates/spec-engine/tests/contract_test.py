#!/usr/bin/env python3
import importlib.util
import os
import re
import unittest
from pathlib import Path

FORBIDDEN = ("std::fs", "std::process", "std::net", "std::env", "std::time", "std::thread", "std::os")
FORBIDDEN_SORTED = tuple(sorted(FORBIDDEN))
IMPORT = re.compile(r"\buse\s+([^;]+);")


def imported(tree):
    """Each path of a `use` tree, with its groups written out and without its new names."""
    tree = "".join(re.sub(r"\s+as\s+\w+", "", tree).split())
    opening = tree.find("{")
    if opening < 0:
        return [tree]
    pieces, depth, start = [], 0, opening + 1
    for index in range(opening, len(tree)):
        if tree[index] == "{":
            depth += 1
        elif tree[index] == "}":
            depth -= 1
        if depth == 0 or (tree[index] == "," and depth == 1):
            pieces.append(tree[start:index])
            start = index + 1
        if depth == 0:
            break
    return [path for piece in pieces if piece for path in imported(tree[:opening] + piece)]


def reaches_outside(source):
    """The forbidden modules a Rust source names, in an import of any shape or in a full path."""
    paths = [path for tree in IMPORT.findall(source) for path in imported(tree)]
    found = {module for module in FORBIDDEN if module in source}
    for path in paths:
        opened = path.removesuffix("::*").removesuffix("::self")
        found.update(module for module in FORBIDDEN if f"{opened}::".startswith(f"{module}::") or module.startswith(f"{opened}::"))
    return sorted(found)


def checkout():
    runfiles = os.environ.get("TEST_SRCDIR")
    if runfiles:
        return Path(runfiles) / os.environ.get("TEST_WORKSPACE", "_main")
    return Path(__file__).resolve().parents[3]


def sources():
    return checkout() / "crates" / "spec-engine" / "src"


class Contract(unittest.TestCase):
    # spec: spec-engine.contract/public-surface-matches-snapshot
    def test_public_surface_matches_the_committed_snapshot(self):
        root = checkout()
        spec = importlib.util.spec_from_file_location("public_surface", root / "tools" / "public_surface.py")
        public_surface = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(public_surface)

        extracted = public_surface.surface(sources())
        committed = (root / "specs" / "spec-engine" / "surface.txt").read_text(encoding="utf-8")

        self.assertEqual(extracted.splitlines(), committed.splitlines())

    # spec: spec-engine.contract/uses-no-other-crate-of-the-workspace
    def test_depends_only_on_its_five_crates(self):
        manifest = (checkout() / "crates" / "spec-engine" / "Cargo.toml").read_text(encoding="utf-8")
        table = re.search(r"^\[dependencies\]\n(.*?)(?=^\[|\Z)", manifest, re.S | re.M)
        self.assertIsNotNone(table, "no [dependencies] table in the manifest")
        dependencies = sorted(re.findall(r"^([\w-]+)\s*[.=]", table.group(1), re.M))

        self.assertEqual(dependencies, ["quick-xml", "serde", "serde_json", "sha2", "toml"])
        self.assertEqual([name for name in dependencies if name.startswith("alondra")], [])

    # spec: spec-engine.contract/reads-only-through-its-caller
    def test_sources_never_reach_the_disk_network_clock_or_other_programs(self):
        found = []
        rust_files = sorted(sources().rglob("*.rs"))
        self.assertNotEqual(rust_files, [], "no Rust sources under crates/spec-engine/src")
        for path in rust_files:
            found += [f"{path.relative_to(checkout())}: {module}" for module in reaches_outside(path.read_text(encoding="utf-8"))]

        self.assertEqual(found, [], "\n".join(found))

    def test_the_search_sees_an_import_of_any_shape(self):
        self.assertEqual(reaches_outside("use std::{fs, process::Command};"), ["std::fs", "std::process"])
        self.assertEqual(reaches_outside("use std::fs as files;"), ["std::fs"])
        self.assertEqual(reaches_outside("use std::{\n    collections::BTreeMap,\n    time::{Duration, Instant},\n};"), ["std::time"])
        self.assertEqual(reaches_outside("use std::*;"), list(FORBIDDEN_SORTED))
        self.assertEqual(reaches_outside("use std as base;"), list(FORBIDDEN_SORTED))
        self.assertEqual(reaches_outside("use std::fs::{self, File};"), ["std::fs"])
        self.assertEqual(reaches_outside("use std::fmt;\nuse stdx::fs;"), [])
        self.assertEqual(reaches_outside("fn now() { std::time::Instant::now(); }"), ["std::time"])
        self.assertEqual(reaches_outside("use std::collections::{BTreeMap, BTreeSet};\nuse std::fmt::Write as _;"), [])


if __name__ == "__main__":
    unittest.main()
