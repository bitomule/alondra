#!/usr/bin/env python3
"""Reports a file in a launcher's place that bazel/env did not install, and a musts older than the pinned one.

tools/bazel runs this before every Bazel command. It only prints; it never replaces anything.
"""
import os
from pathlib import Path
import re
import subprocess
import sys

MARKER = b"alondra-bazel-env-launcher"
VERSION = re.compile(r"\d+\.\d+\.\d+")


def is_launcher(path):
    try:
        return MARKER in path.read_bytes()
    except OSError:
        return False


def kind(path):
    try:
        head = path.read_bytes()[:4]
    except OSError:
        return "unreadable file"
    if head[:2] == b"#!":
        return "script"
    if head in (b"\xcf\xfa\xed\xfe", b"\xca\xfe\xba\xbe", b"\xce\xfa\xed\xfe"):
        return "Mach-O executable"
    return "file"


def reported_version(path):
    try:
        out = subprocess.run([str(path), "--version"], capture_output=True, text=True, timeout=10).stdout
    except (OSError, subprocess.SubprocessError):
        return None
    found = VERSION.search(out)
    return found.group(0) if found else None


def pinned_musts_version(tools_bzl):
    found = re.search(r'^MUSTS_VERSION = "([^"]+)"', tools_bzl.read_text(), re.M)
    return found.group(1) if found else None


def first_on_path(tool, path_dirs):
    for directory in path_dirs:
        candidate = Path(directory) / tool
        if directory and candidate.is_file() and candidate.stat().st_mode & 0o111:
            return candidate
    return None


def problems(tool, target, launcher_source, pinned, path_dirs, version_of=reported_version):
    """One message per file that is not the launcher and would run instead of the pinned tool."""
    candidates = []
    if target.exists():
        candidates.append(target)
    ahead = first_on_path(tool, path_dirs)
    if ahead is not None and ahead not in candidates and not is_launcher(ahead):
        # Only a file ahead of the launcher shadows it.
        if not target.exists() or not is_launcher(target) or path_index(ahead, path_dirs) < path_index(target, path_dirs):
            candidates.append(ahead)
    messages = []
    for path in candidates:
        if is_launcher(path):
            continue
        version = version_of(path) if tool == "musts" else None
        if tool == "musts" and pinned and version == pinned:
            continue
        if tool == "musts":
            seen = f"answers --version {version or 'unknown'}, the repo pins {pinned or 'a version'}"
        else:
            seen = "is not the launcher"
        messages.append(
            f"bazel/env: {path} is a {kind(path)} that bazel/env did not install and {seen}; "
            f"`{tool}` runs it instead of the pinned one. It is left untouched. To replace it, keep a backup:\n"
            f"    mv '{path}' '{path}.bak' && install -m 755 '{launcher_source}' '{target}'"
        )
    return messages


def path_index(path, path_dirs):
    parent = str(path.parent)
    return path_dirs.index(parent) if parent in path_dirs else len(path_dirs)


def main(argv):
    root = Path(argv[1])
    bin_dir = Path(argv[2])
    path_dirs = os.environ.get("PATH", "").split(":")
    pinned = pinned_musts_version(root / "bazel/env/tools.bzl")
    for tool in argv[3:]:
        for message in problems(tool, bin_dir / tool, root / "bazel/env/launcher.sh", pinned, path_dirs):
            print(message, file=sys.stderr)


if __name__ == "__main__":
    main(sys.argv)
