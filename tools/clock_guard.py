#!/usr/bin/env python3
"""The daemon reads the time, waits and starts threads only through `alondra_core::clock`, so a test
can run it on a virtual clock. Production code of `alondrad`, the `daemon-engine*` crates,
`engine-kernel`, `engine-admission`, `engine-github`, `engine-orchestrator` and `daemon-support` never
calls the system clock, a timer or a timed wait directly, under its own name or under an alias.

Test code is outside the guard: files under `tests/`, `*_tests.rs`, `tests.rs`, `test_*.rs`, and the
one item each `#[cfg(test)]` marks. Comments and the text of literals are not code. The guard has
no list of exceptions."""
import os
import re
import sys
from pathlib import Path

CRATES = [
    "alondrad",
    "daemon-engine",
    "daemon-engine-machine",
    "engine-github",
    "engine-kernel",
    "engine-orchestrator",
    "engine-admission",
    "daemon-engine-model",
    "daemon-engine-store",
    "daemon-support",
]

TOKIO_TIMERS = ["sleep", "sleep_until", "interval", "interval_at", "timeout", "timeout_at", "Instant"]

FORBIDDEN_PATHS = [
    (("std", "time", "SystemTime", "now"), "SystemTime::now: use clock::system_time() or clock::now_millis()"),
    (("std", "time", "SystemTime", "elapsed"), "SystemTime::elapsed: use clock::system_time()"),
    (("std", "time", "Instant"), "std::time::Instant: use clock::Instant"),
    (("std", "thread", "sleep"), "thread::sleep: use clock::blocking_sleep()"),
    (("std", "thread", "spawn"), "thread::spawn: use clock::spawn_thread(), which hands the thread the clock"),
    (("std", "thread", "Builder"), "thread::Builder: use clock::spawn_thread(), which hands the thread the clock"),
    (("std", "thread", "scope"), "thread::scope: use clock::scope(), whose threads keep the clock"),
    (("std", "thread", "park_timeout"), "thread::park_timeout: a real wait; wait through clock"),
] + [(("tokio", "time", name), "a tokio timer: use the same function of clock") for name in TOKIO_TIMERS]

FORBIDDEN_WORDS = [
    (r"\bSystemTime\s*::\s*now\b", "SystemTime::now: use clock::system_time() or clock::now_millis()"),
    (r"\bInstant\s*::\s*now\b", "Instant::now: use clock::instant()"),
    (r"\bthread\s*::\s*sleep\b", "thread::sleep: use clock::blocking_sleep()"),
    (r"\bthread\s*::\s*spawn\b", "thread::spawn: use clock::spawn_thread(), which hands the thread the clock"),
    (r"\bthread\s*::\s*Builder\b", "thread::Builder: use clock::spawn_thread(), which hands the thread the clock"),
    (r"\bthread\s*::\s*scope\b", "thread::scope: use clock::scope(), whose threads keep the clock"),
    (r"\.\s*recv_timeout\s*\(", "recv_timeout waits in real time: use clock::recv_timeout()"),
    (r"\.\s*recv_deadline\s*\(", "recv_deadline waits in real time: use clock::recv_timeout()"),
    (r"\.\s*wait_timeout(_while)?\s*\(", "a condvar's timed wait is real time: wait through clock"),
    (r"\.\s*elapsed\s*\(", ".elapsed() reads the clock on its own: use clock::since()"),
    (r"\b(Instant|SystemTime)\s*::\s*elapsed\b", "elapsed reads the clock on its own: use clock::since()"),
    (r"\bReceiver\s*::\s*recv_(timeout|deadline)\b", "recv_timeout waits in real time: use clock::recv_timeout()"),
]


def is_test_file(path):
    parts = path.parts
    name = path.name
    return "tests" in parts or name.endswith("_tests.rs") or name == "tests.rs" or name.startswith("test_") or "testkit" in parts


START = re.compile(r"""//|/\*|(?<![\w])b?r#*"|"|'""")
CHAR = re.compile(r"'(\\(x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]{1,6}\}|.)|[^\\'\n])'")
STRING_END = re.compile(r'\\.|"', re.DOTALL)


def blank(text):
    """The text with every comment and the inside of every literal turned into spaces, lines kept."""
    out, i, n = [], 0, len(text)

    def kept(start, end):
        out.append(text[start:end])

    def cleared(start, end):
        out.append(re.sub(r"[^\n]", " ", text[start:end]))

    while (found := START.search(text, i)) is not None:
        at, token = found.start(), found.group(0)
        kept(i, at)
        if token == "//":
            end = text.find("\n", at)
            end = n if end < 0 else end
            cleared(at, end)
        elif token == "/*":
            depth, end = 1, at + 2
            while end < n and depth:
                if text.startswith("/*", end):
                    depth, end = depth + 1, end + 2
                elif text.startswith("*/", end):
                    depth, end = depth - 1, end + 2
                else:
                    end += 1
            cleared(at, end)
        elif token == "'":
            char = CHAR.match(text, at)
            end = char.end() if char else at + 1
            kept(at, at + 1)
            cleared(at + 1, max(at + 1, end - 1))
            kept(max(at + 1, end - 1), end)
        elif token == '"':
            end = at + 1
            while (piece := STRING_END.search(text, end)) is not None and piece.group(0) != '"':
                end = piece.end()
            close = piece.end() if piece else n
            kept(at, at + 1)
            cleared(at + 1, close - 1)
            kept(close - 1, close)
            end = close
        else:
            close = '"' + "#" * token.count("#")
            inside = found.end()
            stop = text.find(close, inside)
            stop = n if stop < 0 else stop
            kept(at, inside)
            cleared(inside, stop)
            kept(stop, min(n, stop + len(close)))
            end = min(n, stop + len(close))
        i = end
    kept(i, n)
    return "".join(out)


def item_end(code, start):
    """Where the item that starts at `start` ends: its `;` or the brace that closes its body."""
    depth, i, n = 0, start, len(code)
    while i < n:
        c = code[i]
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
            if depth == 0 and c == "}":
                return i + 1
        elif c == ";" and depth == 0:
            return i + 1
        i += 1
    return n


def closing(code, start):
    """Where the bracket that opens at `start` closes, past it."""
    depth = 0
    for i in range(start, len(code)):
        if code[i] in "([{":
            depth += 1
        elif code[i] in ")]}":
            depth -= 1
            if depth == 0:
                return i + 1
    return len(code)


def production_code(text):
    """The code of a file without its comments, the text of its literals and each item a
    `#[cfg(test)]` marks, with every line where it was."""
    code = blank(text)
    out = list(code)
    for marker in re.finditer(TEST_MARK, code):
        start = marker.end()
        while (attribute := re.match(r"\s*#\s*\[", code[start:])):
            start = closing(code, start + attribute.end() - 1)
        end = item_end(code, start)
        for k in range(marker.start(), end):
            if out[k] != "\n":
                out[k] = " "
    return "".join(out)


def use_trees(tree, prefix=()):
    """Every (path, bound name) a `use` tree brings in; a glob brings in nothing by name."""
    tree = tree.strip()
    if not tree:
        return []
    if tree.endswith("}"):
        brace = tree.index("{")
        head = [s.strip() for s in tree[:brace].split("::") if s.strip()]
        inner, parts, depth, current = tree[brace + 1:-1], [], 0, ""
        for c in inner:
            if c == "{":
                depth += 1
            elif c == "}":
                depth -= 1
            if c == "," and depth == 0:
                parts.append(current)
                current = ""
            else:
                current += c
        parts.append(current)
        return [found for part in parts for found in use_trees(part, prefix + tuple(head))]
    pieces = re.split(r"\s+as\s+", tree, maxsplit=1)
    path, alias = pieces[0], (pieces[1].strip() if len(pieces) > 1 else None)
    segments = [s.strip() for s in path.split("::") if s.strip()]
    if segments and segments[-1] == "self":
        segments = segments[:-1]
    full = prefix + tuple(segments)
    if not full or full[-1] == "*":
        return [(full, None)]
    return [(full, alias if alias is not None else full[-1])]


def canonical(path):
    path = tuple(segment for segment in path if segment)
    if path and path[0] == "core":
        path = ("std",) + path[1:]
    return path


def forbidden(path):
    path = canonical(path)
    for prefix, why in FORBIDDEN_PATHS:
        if path[: len(prefix)] == prefix:
            return why
    return None


PATH = re.compile(r"(?:\b|::)[A-Za-z_]\w*(?:\s*::\s*[A-Za-z_]\w*)+")


def problems_in(code):
    found = {}

    def report(offset, why):
        found.setdefault(code[:offset].count("\n") + 1, why)

    aliases = {}
    for use in re.finditer(r"\buse\s+([^;]*);", code):
        for path, name in use_trees(use.group(1)):
            why = forbidden(path)
            if why:
                report(use.start(), why)
            if name and name != "_":
                aliases[name] = path
    for definition in re.finditer(r"\btype\s+(\w+)\s*(?:<[^=]*>)?\s*=\s*([\w:\s]+?)\s*(?:<|;)", code):
        aliases[definition.group(1)] = tuple(s.strip() for s in definition.group(2).split("::") if s.strip())

    def resolve(segments, seen=()):
        head = segments[0]
        if head in aliases and head not in seen:
            return resolve(list(aliases[head]) + segments[1:], seen + (head,))
        return segments

    without_uses = re.sub(r"\buse\s+[^;]*;", lambda use: re.sub(r"[^\n]", " ", use.group(0)), code)
    for match in PATH.finditer(without_uses):
        segments = [s.strip() for s in match.group(0).split("::") if s.strip()]
        if segments[0] in ("crate", "self", "super"):
            continue
        why = forbidden(resolve(segments))
        if why:
            report(match.start(), why)
    for pattern, why in FORBIDDEN_WORDS:
        for match in re.finditer(pattern, code):
            report(match.start(), why)
    return found


TEST_MARK = r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]"


def absolute(path):
    return Path(os.path.normpath(os.path.abspath(path)))


def declared_modules(path, text):
    """The files of the modules `path` declares with `mod name;`, each with whether a
    `#[cfg(test)]` marks it."""
    code = blank(text)
    directory = path.parent if path.name in ("lib.rs", "main.rs", "mod.rs") else path.parent / path.stem
    for declaration in re.finditer(r"((?:#\s*\[[^\]]*\]\s*)*)(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+(\w+)\s*;", code):
        attributes = text[declaration.start(1): declaration.end(1)]
        marked = re.search(TEST_MARK, code[declaration.start(1): declaration.end(1)]) is not None
        given = re.search(r'path\s*=\s*"([^"]+)"', attributes)
        name = declaration.group(2)
        if given:
            yield path.parent / given.group(1), marked
        else:
            yield directory / f"{name}.rs", marked
            yield directory / name / "mod.rs", marked


def sources(root):
    for crate in CRATES:
        base = Path(root) / "crates" / crate
        files = sorted(absolute(path) for path in base.rglob("*.rs"))
        test_only = {path for path in files if is_test_file(path.relative_to(absolute(base)))}
        changed = True
        while changed:
            changed = False
            for path in files:
                for module, marked in declared_modules(path, path.read_text()):
                    module = absolute(module)
                    if (marked or path in test_only) and module.exists() and module not in test_only:
                        test_only.add(module)
                        changed = True
        for path in files:
            if path not in test_only:
                yield path


def problems(root):
    root = absolute(root)
    found = []
    for path in sources(root):
        for number, why in sorted(problems_in(production_code(path.read_text())).items()):
            found.append(f"{path.relative_to(root)}:{number}: {why}")
    return found


if __name__ == "__main__":
    found = problems(sys.argv[1] if len(sys.argv) > 1 else ".")
    print("\n".join(found))
    sys.exit(1 if found else 0)
