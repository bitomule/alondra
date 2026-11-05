#!/usr/bin/env python3
"""Keeps each bazel/test check's `paths` equal to the files its targets read.

`check` asks Bazel for the source, BUILD and .bzl files behind each check's targets and fails when one
of them is not matched by the check's `paths`, so that change would not reopen the tests that read it,
or when a glob under a directory matches nothing they read, so a change there would reopen tests it
cannot affect. Root files such as `.bazelrc` are read by Bazel itself and no query names them, and a
`**/` glob belongs to a test that reads the checkout through its runfiles link.
`propose CHECK` prints the `paths` Bazel's answer needs. Only `bazel query` runs; nothing is built."""
import argparse
import re
import subprocess
import sys
from pathlib import Path

CRATES_ROOT = "crates/"
CHECK_KEY = re.compile(r"^  ([\w-]+):\s*$")
ITEM = re.compile(r'^\s+-\s+"?([^"#]+?)"?\s*(?:#.*)?$')


def glob_regex(glob):
    out, i = "", 0
    while i < len(glob):
        if glob.startswith("**/", i):
            out += "(?:.*/)?"
            i += 3
        elif glob.startswith("**", i):
            out += ".*"
            i += 2
        elif glob[i] == "*":
            out += "[^/]*"
            i += 1
        elif glob[i] == "?":
            out += "[^/]"
            i += 1
        else:
            out += re.escape(glob[i])
            i += 1
    return re.compile("^" + out + "$", re.IGNORECASE)


def matches(globs, path):
    return any(glob_regex(glob).match(path) for glob in globs)


def parse_checks(text):
    checks, current, section = {}, None, None
    for line in text.splitlines():
        if line.lstrip().startswith("#") or not line.strip():
            continue
        key = CHECK_KEY.match(line)
        if key:
            current = checks.setdefault(key.group(1), {"uses": "", "paths": [], "targets": []})
            section = None
            continue
        if current is None:
            continue
        stripped = line.strip()
        if stripped.startswith("uses:"):
            current["uses"] = stripped.split(":", 1)[1].strip()
        elif stripped in ("paths:", "targets:"):
            section = stripped[:-1]
        elif stripped.endswith(":") or ":" in stripped.split()[0]:
            section = None
        elif section and (item := ITEM.match(line)):
            current[section].append(item.group(1))
    return {name: check for name, check in checks.items() if check["uses"] == "bazel/test"}


def label_path(label):
    package, _, name = label[2:].partition(":")
    return f"{package}/{name}" if package else name


def closure(targets):
    deps = " + ".join(f"deps({target})" for target in targets)
    query = f'filter("^//", kind("source file", {deps}) + buildfiles({deps}))'
    result = subprocess.run(
        ["bazelisk", "query", query, "--output=label", "--keep_going"],
        capture_output=True, text=True,
    )
    if result.returncode != 0:
        raise SystemExit(f"bazel query failed for {targets}:\n{result.stderr}")
    return {label_path(line) for line in result.stdout.splitlines() if line.startswith("//")}


def tracked_files():
    listing = subprocess.run(["git", "ls-files"], capture_output=True, text=True, check=True)
    return listing.stdout.splitlines()


def propose(files, tracked):
    """Globs for `files`: a whole crate directory when one of its files is read, else the smallest
    globs that match the files and nothing tracked outside them."""
    wanted = set(files)
    crates = {"/".join(path.split("/")[:2]) for path in wanted if path.startswith(CRATES_ROOT) and path.count("/") > 1}
    wanted |= {path for path in tracked if any(path.startswith(crate + "/") for crate in crates)}
    below = {}
    for path in tracked:
        parts = path.split("/")
        for depth in range(1, len(parts)):
            below.setdefault("/".join(parts[:depth]), []).append(path)
    globs = []
    covered = set()
    for directory in sorted(below, key=lambda d: d.count("/")):
        if any(directory.startswith(prefix + "/") for prefix in covered):
            continue
        if all(path in wanted for path in below[directory]):
            globs.append(f"{directory}/**")
            covered.add(directory)
    remaining = sorted(path for path in wanted if not any(path.startswith(d + "/") for d in covered))
    return sorted(globs) + by_extension(remaining, tracked, wanted)


def by_extension(paths, tracked, wanted):
    """`dir/*.ext` for every extension whose files in `dir` are all wanted; the rest stay one by one."""
    siblings = {}
    for path in tracked:
        directory, _, name = path.rpartition("/")
        siblings.setdefault((directory, Path(name).suffix), []).append(path)
    out = set()
    for path in paths:
        directory, _, name = path.rpartition("/")
        suffix = Path(name).suffix
        group = siblings[(directory, suffix)]
        if suffix and len(group) > 1 and all(member in wanted for member in group):
            out.add(f"{directory}/*{suffix}" if directory else f"*{suffix}")
        else:
            out.add(path)
    return sorted(out)


def dead_globs(paths, needed):
    return [glob for glob in paths if "/" in glob and not glob.startswith("**/")
            and not any(glob_regex(glob).match(path) for path in needed)]


def check(manifest, tracked, selected):
    failed = False
    for name, spec in parse_checks(manifest).items():
        if selected and name not in selected:
            continue
        needed = closure(spec["targets"])
        lost = sorted(path for path in needed if not matches(spec["paths"], path))
        unread = sorted(path for path in tracked if matches(spec["paths"], path) and path not in needed)
        dead = dead_globs(spec["paths"], needed)
        print(f"{name}: reads {len(needed)} files, {len(lost)} missing from paths, {len(unread)} in paths but unread, {len(dead)} dead globs")
        for path in lost:
            print(f"  missing  {path}")
        for glob in dead:
            print(f"  dead     {glob}")
        failed = failed or bool(lost) or bool(dead)
    return failed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["check", "propose"])
    parser.add_argument("checks", nargs="*")
    parser.add_argument("--manifest", default="MUSTS.yml")
    args = parser.parse_args()
    manifest = Path(args.manifest).read_text()
    if args.command == "check":
        sys.exit(1 if check(manifest, tracked_files(), set(args.checks)) else 0)
    checks = parse_checks(manifest)
    for name in args.checks:
        print(f"{name}:")
        for glob in propose(closure(checks[name]["targets"]), tracked_files()):
            print(f'  - "{glob}"')


if __name__ == "__main__":
    main()
