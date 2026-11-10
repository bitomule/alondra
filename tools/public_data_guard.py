#!/usr/bin/env python3
"""Fails when a tracked file carries personal or private data that must not be public.

This repository is public: everything committed here can be read by anyone. The guard scans every
tracked text file (or only the lines a diff adds, with --against) for personal emails, home
directories of real people, device identifiers, public IP addresses, credentials and tracker ids. A match is either in the allowlist next to this file (with the
reason it is harmless) or a failure; an allowlist entry that matches nothing is a failure too, so
the list only shrinks.

Literals that must not be written down in a public file at all (the maintainer's name, names of
private apps or repositories, hosts) are not in this repository: they go in a file outside the
checkout that ALONDRA_PRIVATE_LITERALS points to, one case-insensitive literal per line. A finding
from that file never prints the literal. When the variable is unset or the file is missing, as on a
contributor's Mac, the generic rules above still run and the check passes or fails on them alone.
"""
import ipaddress
import os
import re
import subprocess
import sys
from pathlib import Path

ALLOWLIST = "tools/public_data_allowlist.tsv"
ANY = "*"
PRIVATE_RULE = "private-literal"
PRIVATE_LITERALS_ENV = "ALONDRA_PRIVATE_LITERALS"
OWN_FILES = {
    "tools/public_data_guard.py",
    "tools/public_data_allowlist.tsv",
    "tools/tests/test_public_data_guard.py",
}
SKIPPED_DIRS = {".git", ".claude", ".scratch", ".musts", "node_modules", "target", "DerivedData", "dist"}
SKIPPED_NAMES = {"package-lock.json", "MODULE.bazel.lock", "Cargo.lock", "Package.resolved", "LICENSE", "LICENSE.md"}
MAX_BYTES = 2_000_000

RESERVED_EMAIL_DOMAINS = (
    "example.com", "example.org", "example.net", "example", "test", "invalid", "localhost", "local",
    "acme.com", "ejemplo.com", "getalondra.com", "anthropic.com", "noreply.github.com",
)
GENERIC_EMAIL_USERS = {"git", "noreply", "no-reply"}
GENERIC_HOMES = {
    "ana", "d", "shared", "alice", "bob", "me", "you", "user", "name", "someone", "example", "runner",
    "tester", "test", "dev", "owner", "home", "synthetic", "work", "nobody", "root", "alondra",
    "david", "admin", "x", "other", "fixture", "u", "ana.lopez",
}
PLACEHOLDER = ("example", "fixture", "fake", "synthetic", "dummy", "placeholder", "loopback")
PUBLIC_ADDRESSES = {"8.8.8.8", "8.8.4.4", "1.1.1.1", "1.0.0.1", "9.9.9.9", "2001:4860:4860::8888"}

EMAIL = re.compile(r"(?<![\w.+/-])([A-Za-z0-9._+-]+)@((?:[A-Za-z0-9-]+\.)+[A-Za-z]{2,})\b")
HOME = re.compile(r"/Users/([A-Za-z0-9._-]+)")
IPV4 = re.compile(r"(?<![\w.])(\d{1,3}(?:\.\d{1,3}){3})(?![\w.]*\d)(?!\.\d)")
OCTETS = re.compile(r"Ipv4Addr::new\((\d{1,3}),\s*(\d{1,3}),\s*(\d{1,3}),\s*(\d{1,3})\)")
IPV6 = re.compile(r"(?<![\w:.])([0-9A-Fa-f]{0,4}(?::[0-9A-Fa-f]{0,4}){2,7})(?![\w:.])")
DEVICE_LINE = re.compile(r"udid|hardware[_ -]?uuid|serial[_ -]?number|provisioning", re.I)
UUID = re.compile(r"\b[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}\b"
                  r"|\b[0-9A-Fa-f]{8}-[0-9A-Fa-f]{16}\b|\b[0-9a-f]{40}\b")
SECRETS = [
    re.compile(r"\bgh[pousr]_[A-Za-z0-9]{36,}\b"),
    re.compile(r"\bgithub_pat_[A-Za-z0-9_]{40,}\b"),
    re.compile(r"\bxox[abprs]-[A-Za-z0-9-]{10,}\b"),
    re.compile(r"\bAKIA[0-9A-Z]{16}\b"),
    re.compile(r"\bsk-ant-[A-Za-z0-9_-]{20,}\b"),
    re.compile(r"\bsk-[A-Za-z0-9]{32,}\b"),
    re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
]
PRIVATE_NAMES = [
    ("tracker-id", re.compile(r"\bBIT-\d+\b")),
]


def synthetic_id(text):
    digits = text.replace("-", "").lower()
    return len(set(digits)) <= 3 or digits.startswith(("12345678", "abcdef", "deadbeef"))


def hits(line, literals):
    """Every (rule, matched text) a single line carries."""
    found = []
    for m in EMAIL.finditer(line):
        user, domain = m.group(1).lower(), m.group(2).lower()
        reserved = any(domain == d or domain.endswith("." + d) for d in RESERVED_EMAIL_DOMAINS)
        throwaway = len(domain.split(".")[0]) <= 1
        if not reserved and not throwaway and user not in GENERIC_EMAIL_USERS:
            found.append(("personal-email", m.group(0)))
    for m in HOME.finditer(line):
        if m.group(1).lower() not in GENERIC_HOMES and not m.group(1).startswith(("$", "{")):
            found.append(("home-path", m.group(0)))
    spelled = [(m.group(0), ".".join(m.groups())) for m in OCTETS.finditer(line)]
    for m in IPV4.finditer(line):
        spelled.append((m.group(1), m.group(1)))
    for shown, text in spelled:
        try:
            ip = ipaddress.IPv4Address(text)
        except ValueError:
            continue
        if ip.is_global and text not in PUBLIC_ADDRESSES and not text.startswith(("0.", "255.")):
            found.append(("public-ip", text))
    for m in IPV6.finditer(line):
        try:
            ip = ipaddress.IPv6Address(m.group(1))
        except ValueError:
            continue
        if ip.is_global and m.group(1) not in PUBLIC_ADDRESSES:
            found.append(("public-ip", m.group(1)))
    if DEVICE_LINE.search(line):
        for m in UUID.finditer(line):
            if not synthetic_id(m.group(0)):
                found.append(("device-id", m.group(0)))
    for pattern in SECRETS:
        for m in pattern.finditer(line):
            if not any(marker in m.group(0).lower() for marker in PLACEHOLDER):
                found.append(("secret", m.group(0)))
    for rule, pattern in PRIVATE_NAMES:
        for m in pattern.finditer(line):
            found.append((rule, m.group(0)))
    lowered = line.lower()
    for literal in literals:
        if literal in lowered:
            found.append(("private-literal", ANY))
    return found


def private_literals():
    path = os.environ.get(PRIVATE_LITERALS_ENV)
    if not path or not Path(path).is_file():
        return []
    lines = Path(path).read_text().splitlines()
    return [line.strip().lower() for line in lines if line.strip() and not line.startswith("#")]


def tracked(root):
    git = subprocess.run(["git", "-C", str(root), "ls-files", "-z"], capture_output=True)
    if git.returncode == 0 and git.stdout:
        return [p for p in git.stdout.decode().split("\0") if p]
    files = []
    for directory, names, filenames in os.walk(root):
        names[:] = [n for n in names if n not in SKIPPED_DIRS and not n.startswith("bazel-")]
        files += [str(Path(directory, f).relative_to(root)) for f in filenames]
    return files


def skipped(relative):
    parts = Path(relative).parts
    return (relative in OWN_FILES or Path(relative).name in SKIPPED_NAMES
            or any(part in SKIPPED_DIRS for part in parts))


def lines_of(root, relative):
    path = Path(root, relative)
    try:
        if not path.is_file() or path.stat().st_size > MAX_BYTES:
            return []
        return path.read_text(encoding="utf-8").splitlines()
    except (UnicodeDecodeError, OSError):
        return []


def added_lines(root, base):
    """The lines a diff against `base` adds, by file."""
    diff = subprocess.run(["git", "-C", str(root), "diff", "-U0", "--no-color", base, "--"],
                          capture_output=True, text=True, check=True).stdout
    added, current = {}, None
    for line in diff.splitlines():
        if line.startswith("+++ b/"):
            current = line[6:]
            added[current] = []
        elif line.startswith("+") and not line.startswith("+++") and current:
            added[current].append(line[1:])
    return added


def load_allowlist(root):
    path = Path(root, ALLOWLIST)
    entries = {}
    if path.is_file():
        for line in path.read_text().splitlines():
            if line.strip() and not line.startswith("#"):
                parts = line.split("\t")
                if len(parts) == 4 and parts[3].strip():
                    entries[(parts[0], parts[1], parts[2])] = parts[3]
    return entries


def report(root, against=None):
    root = Path(root)
    literals = private_literals()
    allowlist = load_allowlist(root)
    used, problems = set(), []
    if against:
        sources = {p: ls for p, ls in added_lines(root, against).items() if not skipped(p)}
    else:
        sources = {p: lines_of(root, p) for p in tracked(root) if not skipped(p)}
    for relative in sorted(sources):
        for number, line in enumerate(sources[relative], 1):
            for rule, text in hits(line, literals):
                key = next((k for k in ((relative, rule, text), (relative, rule, ANY)) if k in allowlist), None)
                if key:
                    used.add(key)
                else:
                    problems.append(f"{relative}:{number}: {rule}: {text}")
    if not against:
        unverifiable = {key for key in allowlist if key[1] == PRIVATE_RULE and not literals}
        for key in sorted(set(allowlist) - used - unverifiable):
            problems.append(f"{ALLOWLIST}: unused entry {'  '.join(key)}")
    return problems


def main(argv):
    against = argv[argv.index("--against") + 1] if "--against" in argv else None
    root = Path(__file__).resolve().parents[1]
    problems = report(root, against)
    for problem in problems:
        print(problem)
    if problems:
        print(f"\n{len(problems)} finding(s). Replace the data with a synthetic value, or allow it in "
              f"{ALLOWLIST} with the reason it is harmless.")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
