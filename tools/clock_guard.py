#!/usr/bin/env python3
"""The daemon reads the time and waits only through `alondra_core::clock`, so a test can run it on a
virtual clock. Production code of `alondrad`, the `daemon-engine*` crates, `engine-kernel` and `daemon-support` never
calls the system clock or a timer directly.

Test code is outside the guard: files under `tests/`, `*_tests.rs`, `tests.rs`, `test_*.rs`, and
everything after the first `#[cfg(test)]` of a file. The guard has no list of exceptions."""
import re
import sys
from pathlib import Path

CRATES = [
    "alondrad",
    "daemon-engine",
    "daemon-engine-machine",
    "engine-kernel",
    "daemon-engine-model",
    "daemon-engine-store",
    "daemon-support",
]

FORBIDDEN = [
    (r"\bSystemTime::now\b", "SystemTime::now: use clock::system_time() or clock::now_millis()"),
    (r"\bInstant::now\b", "Instant::now: use clock::instant()"),
    (r"\bthread::sleep\b", "thread::sleep: use clock::blocking_sleep()"),
    (r"\btokio::time::(sleep|sleep_until|interval|interval_at|timeout|timeout_at|Instant)\b", "a tokio timer: use the same function of clock"),
    (r"\buse\s+tokio::time::[^;]*\b(sleep|sleep_until|interval|interval_at|timeout|timeout_at|Instant)\b", "importing a tokio timer: use clock"),
    (r"\buse\s+std::time::[^;]*\bInstant\b", "importing std Instant: use clock::Instant"),
    (r"\bstd::time::Instant\b", "std::time::Instant: use clock::Instant"),
]


def is_test_file(path):
    parts = path.parts
    name = path.name
    return "tests" in parts or name.endswith("_tests.rs") or name == "tests.rs" or name.startswith("test_") or "testkit" in parts


def production_text(path):
    text = path.read_text()
    marker = re.search(r"^\s*#\[cfg\(test\)\]", text, re.MULTILINE)
    return text[: marker.start()] if marker else text


def sources(root):
    for crate in CRATES:
        base = Path(root) / "crates" / crate
        for path in sorted(base.rglob("*.rs")):
            if not is_test_file(path.relative_to(base)):
                yield path


def without_comments(text):
    return "\n".join(line.split("//")[0] for line in text.split("\n"))


def problems(root):
    found = {}
    for path in sources(root):
        text = without_comments(production_text(path))
        for pattern, why in FORBIDDEN:
            for match in re.finditer(pattern, text):
                number = text[: match.start()].count("\n") + 1
                found.setdefault((str(path.relative_to(root)), number), why)
    return [f"{where}:{number}: {why}" for (where, number), why in sorted(found.items())]


if __name__ == "__main__":
    found = problems(sys.argv[1] if len(sys.argv) > 1 else ".")
    print("\n".join(found))
    sys.exit(1 if found else 0)
