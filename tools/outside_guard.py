#!/usr/bin/env python3
"""The daemon reaches what is outside its process only through `alondra_core::outside`: the programs
it runs (`gh`, `git`, `claude`, the Mac's tools) and the hosts it connects to. A scenario isolates
that door and reports whatever goes through it as a leak, so production code of `alondrad`, the
`daemon-engine*` crates, `engine-kernel`, `daemon-support` and `core` (their `src/`: the launcher
launchd runs to start the daemon is another program) never names a process
command, builds an HTTP client or opens a socket of its own. HTTP clients are built in
`daemon-support/src/web.rs`, which checks every host with the door before it connects.

Test code is outside the guard: files under `tests/`, `*_tests.rs`, `tests.rs`, `test_*.rs`, the
files of modules declared `#[cfg(test)]`, and each item marked `#[cfg(test)]` or
`#[cfg(any(test, feature = "test-hooks"))]`, which ends at its `;` or at the brace that closes its body."""
import re
import sys
from pathlib import Path

CRATES = [
    "alondrad",
    "core",
    "daemon-engine",
    "daemon-engine-machine",
    "engine-kernel",
    "daemon-engine-model",
    "daemon-engine-store",
    "daemon-support",
]

DOORS = {"crates/core/src/outside.rs", "crates/daemon-support/src/web.rs"}

FORBIDDEN = [
    (r"\b(std|tokio)\s*::\s*process\s*::\s*Command\b", "a process command: use outside::Command or outside::AsyncCommand"),
    (r"\b(std|tokio)\s*::\s*process\s*::\s*\{[^}]*\bCommand\b", "importing a process command: use outside::Command or outside::AsyncCommand"),
    (r"\b(std|tokio)\s*::\s*process\s*::\s*\*", "a glob import of a process module: name what you use"),
    (r"\bCommandExt\b", "CommandExt: outside::Command has what a command needs"),
    (r"\.into_std\s*\(\s*\)", "a command taken out of the door: run it through outside::Command"),
    (r"\breqwest\s*::\s*(Client\s*::\s*(builder|new)|ClientBuilder\s*::\s*new|get\s*\()", "an HTTP client of its own: use web::client()"),
    (r"\bClient\s*::\s*builder\s*\(", "an HTTP client of its own: use web::client()"),
    (r"\bTcpStream\s*::\s*connect", "a socket of its own: use outside::tcp_connect_timeout"),
    (r"\bUdpSocket\s*::\s*bind\b", "a socket of its own: use outside::udp_send"),
]


def is_test_file(path):
    parts = path.parts
    name = path.name
    return "tests" in parts or name.endswith("_tests.rs") or name == "tests.rs" or name.startswith("test_") or "testkit" in parts


def item_end(text, start):
    """Where the item that starts at `start` ends: after its `;`, or after the brace that closes its body."""
    depth = 0
    index = start
    while index < len(text):
        character = text[index]
        if character == '"':
            index = text.find('"', index + 1)
            while index > 0 and text[index - 1] == "\\":
                index = text.find('"', index + 1)
            if index < 0:
                return len(text)
        elif character == ";" and depth == 0:
            return index + 1
        elif character == "{":
            depth += 1
        elif character == "}":
            depth -= 1
            if depth == 0:
                return index + 1
        index += 1
    return len(text)


TEST_ONLY = r"#\[cfg\((test|any\(\s*test\s*,\s*feature\s*=\s*\"test-hooks\"\s*\))\)\]"


def production_text(path):
    """The file with every item only tests compile blanked out, its lines kept so numbers hold."""
    text = path.read_text()
    while marker := re.search(TEST_ONLY, text):
        end = item_end(text, marker.end())
        text = text[: marker.start()] + re.sub(r"[^\n]", " ", text[marker.start() : end]) + text[end:]
    return text


def test_modules(path):
    """The files of the modules `path` declares for tests alone: `#[cfg(test)] mod name;`."""
    text = path.read_text()
    own = path.parent if path.name in ("lib.rs", "main.rs", "mod.rs") else path.parent / path.stem
    pattern = TEST_ONLY + r"\s*(#\[path\s*=\s*\"(?P<path>[^\"]+)\"\]\s*)?(pub(\([^)]*\))?\s+)?mod\s+(?P<name>\w+)\s*;"
    for match in re.finditer(pattern, text):
        if match.group("path"):
            yield (path.parent / match.group("path")).resolve()
        else:
            yield (own / f"{match.group('name')}.rs").resolve()
            yield (own / match.group("name") / "mod.rs").resolve()


def sources(root):
    for crate in CRATES:
        base = Path(root) / "crates" / crate
        paths = sorted((base / "src").rglob("*.rs"))
        for_tests = {module for path in paths for module in test_modules(path)}
        for path in paths:
            relative = path.relative_to(root).as_posix()
            if relative not in DOORS and path.resolve() not in for_tests and not is_test_file(path.relative_to(base)):
                yield path


def without_comments(text):
    return "\n".join(re.sub(r"(^|[^:])//.*", r"\1", line) for line in text.split("\n"))


def problems(root):
    found = {}
    for path in sources(root):
        text = without_comments(production_text(path))
        for pattern, why in FORBIDDEN:
            for match in re.finditer(pattern, text):
                number = text[: match.start()].count("\n") + 1
                found.setdefault((path.relative_to(root).as_posix(), number), why)
    return [f"{where}:{number}: {why}" for (where, number), why in sorted(found.items())]


if __name__ == "__main__":
    found = problems(sys.argv[1] if len(sys.argv) > 1 else ".")
    print("\n".join(found))
    sys.exit(1 if found else 0)
