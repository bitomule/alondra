#!/usr/bin/env python3
"""List and compare committed web build inputs, without reading either working tree."""
import argparse
import json
from pathlib import PurePosixPath
import subprocess
import sys

MANIFEST = "web/source/inputs.json"


def git(checkout, *args):
    return subprocess.run(["git", "-C", str(checkout), *args], check=True,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE).stdout


def commit(checkout, ref):
    return git(checkout, "rev-parse", "--verify", "--end-of-options", f"{ref}^{{commit}}").decode().strip()


def paths(checkout, revision):
    manifest = json.loads(git(checkout, "show", f"{revision}:{MANIFEST}"))
    entries = manifest.get("paths") if isinstance(manifest, dict) else None
    if not isinstance(entries, list) or not entries:
        raise ValueError("web/source/inputs.json must contain a non-empty paths list")
    for entry in entries:
        if not isinstance(entry, str):
            raise ValueError("each web source path must be a string")
        path = PurePosixPath(entry)
        if (not entry.startswith("web/") or str(path) != entry or
                any(part in (".", "..") for part in path.parts) or
                any(character.isspace() for character in entry) or "\\" in entry):
            raise ValueError(f"invalid web source path: {entry!r}")
    if len(entries) != len(set(entries)) or any(
            a != b and a.startswith(b + "/") for a in entries for b in entries):
        raise ValueError("web source paths must not overlap or repeat")
    return entries


def tree(checkout, revision):
    entries = {}
    for record in git(checkout, "ls-tree", "-r", "-z", "--full-tree", revision).split(b"\0"):
        if record:
            metadata, name = record.split(b"\t", 1)
            mode, kind, digest = metadata.decode().split()
            entries[name.decode()] = (mode, kind, digest)
    return entries


def selected(entries, roots):
    return {name: value for name, value in entries.items()
            if any(name == root or name.startswith(root + "/") for root in roots)}


def source_inputs(checkout, revision):
    roots = paths(checkout, revision)
    inputs = selected(tree(checkout, revision), roots)
    for root in roots:
        if not any(name == root or name.startswith(root + "/") for name in inputs):
            raise ValueError(f"missing source input: {root}")
    for name, (mode, kind, _) in inputs.items():
        if kind != "blob" or mode not in ("100644", "100755"):
            raise ValueError(f"web input must be a regular file: {name}")
    return roots, inputs


def check(source, source_revision, peer, peer_revision, mode):
    roots, expected = source_inputs(source, source_revision)
    actual = selected(tree(peer, peer_revision), roots)
    if mode == "absent":
        return [f"duplicate: {name}" for name in sorted(actual)]
    differences = []
    for name in sorted(expected.keys() | actual.keys()):
        if name not in actual:
            differences.append(f"missing: {name}")
        elif name not in expected:
            differences.append(f"extra: {name}")
        elif expected[name] != actual[name]:
            differences.append(f"changed: {name}")
    return differences


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("paths", "check"):
        command = commands.add_parser(name)
        command.add_argument("--source", required=True)
        command.add_argument("--source-ref", default="HEAD")
        if name == "check":
            command.add_argument("--peer", required=True)
            command.add_argument("--peer-ref", default="HEAD")
            command.add_argument("--mode", choices=("mirror", "absent"), required=True)
    args = parser.parse_args(argv)
    try:
        revision = commit(args.source, args.source_ref)
        if args.command == "paths":
            roots, _ = source_inputs(args.source, revision)
            print("\n".join(roots))
            return 0
        peer_revision = commit(args.peer, args.peer_ref)
        differences = check(args.source, revision, args.peer, peer_revision, args.mode)
        print(f"Web source {revision}; peer {peer_revision}; mode {args.mode}")
        if differences:
            print("\n".join(differences))
            return 1
        print("Web source check passed")
        return 0
    except (ValueError, subprocess.CalledProcessError, OSError) as error:
        if isinstance(error, subprocess.CalledProcessError):
            print(f"Cannot read committed web inputs: git exited {error.returncode}", file=sys.stderr)
        else:
            print(str(error), file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
