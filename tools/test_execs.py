#!/usr/bin/env python3
"""The programs each test may run besides itself, read from the TEST_EXECS of every test target.

The test wrapper (`tools/test-tmpdir.sh`) refuses every exec of a test but its own executable and
the interpreter of its script; TEST_EXECS lists, one entry per program, what else the target's
tests run. An entry is a system path (`/usr/bin/git`), a `$(rootpath <label>)` of an executable
the target declares in `data`, `path:<name>` for the program of that name the test's PATH finds,
`tmpdir` for programs the test writes under its own temporary folders, or alone `own-sandbox` (the test starts its own sandbox, which cannot run inside the
guard's) or `e2e` (a `manual` target tagged `e2e`, which only the release runs).

`//tools:test_execs_test` fails on a malformed entry, a program with no reason in
`tools/test_execs.tsv`, a reason no target uses, a total above its ceiling, or a table in
`docs/running-tests.md` that is not the one the entries make; `bazel run //tools:test_execs -- write`
rewrites that table from the XML of `//tools/layers:rules`.
"""
import collections
import os
import re
import sys
import xml.etree.ElementTree as ElementTree
from pathlib import Path

# Entries of every TEST_EXECS but `own-sandbox` and `e2e`. It only goes down: lower it with every
# program a test stops running.
EXEC_CEILING = 203
# Targets whose tests start their own sandbox and so run without the guard. Only goes down too.
OWN_SANDBOX_CEILING = 5

VARIABLE = "TEST_EXECS"
TEMPORARY = "tmpdir"
OWN_SANDBOX = "own-sandbox"
E2E = "e2e"
REASONS = "tools/test_execs.tsv"
E2E_REASON = "End to end: runs without the guard, only when the release names it in `RELEASE_ONLY_TESTS` or on request."
RULES = "tools/layers/rules"
DOC = "docs/running-tests.md"
BEGIN = "<!-- test-execs:begin -->"
END = "<!-- test-execs:end -->"
UNGUARDED = {
    "`//apple/...`": "Swift tests drive simulators, `xcodebuild` and the Xcode tools; they run without the guard.",
    "`//tests/...`": "The opt-in suites run real daemons, relays and the native Claude on request; they run without the guard.",
    "every `rust_doc_test`": "rustdoc compiles each example with the C toolchain and runs it from its own temporary folder; doctests run without the guard.",
}
SYSTEM = re.compile(r"^/(?:bin|sbin|usr/bin|usr/sbin|usr/libexec)/[\w.+-]+$")
ROOTPATH = re.compile(r"^\$\(rootpath ([^)\s]+)\)$")
ON_PATH = re.compile(r"^path:[\w.+-]+$")
ENTRY = re.compile(r"\$\([^)]*\)|\S+")


def canonical(label, package):
    if label.startswith(":"):
        return f"//{package}{label}"
    if label.startswith("//") and ":" not in label:
        return f"{label}:{label.rsplit('/', 1)[-1]}"
    return label


def read_tests(path):
    tests = {}
    for rule in ElementTree.parse(path).getroot().iter("rule"):
        kind = rule.get("class")
        name = rule.get("name")
        if not kind.endswith("_test") or not name.startswith("//"):
            continue
        attributes = {child.get("name"): child for child in rule}
        env = {}
        if "env" in attributes:
            for pair in attributes["env"].iter("pair"):
                key, value = [item.get("value") for item in pair]
                env[key] = value
        tags = [item.get("value") for item in attributes["tags"].iter("string")] if "tags" in attributes else []
        data = [item.get("value") for item in attributes["data"].iter("label")] if "data" in attributes else []
        tests[name] = {"kind": kind, "execs": env.get(VARIABLE), "tags": tags, "data": data}
    return tests


def entries_of(value):
    return ENTRY.findall(value or "")


def program_of(entry, package):
    found = ROOTPATH.match(entry)
    return canonical(found.group(1), package) if found else entry


def problems_of(label, test):
    package = label[2:].split(":")[0]
    entries = entries_of(test["execs"])
    found = []
    if test["execs"] is not None and not entries:
        found.append(f"{label}: {VARIABLE} is empty; remove it.")
    if E2E in test["tags"]:
        if entries != [E2E]:
            found.append(f"{label} is tagged {E2E}: its {VARIABLE} is `{E2E}` alone.")
        if "manual" not in test["tags"]:
            found.append(f"{label} is tagged {E2E}: it is `manual` too, so only the release runs it.")
    elif E2E in entries:
        found.append(f"{label}: only a target tagged {E2E} turns the guard off with `{E2E}`.")
    if OWN_SANDBOX in entries and entries != [OWN_SANDBOX]:
        found.append(f"{label}: `{OWN_SANDBOX}` turns the guard off, so it is the only entry.")
    if len(set(entries)) != len(entries):
        found.append(f"{label}: {VARIABLE} names a program twice.")
    data = {canonical(item, package) for item in test["data"]}
    for entry in entries:
        rootpath = ROOTPATH.match(entry)
        if entry in (TEMPORARY, OWN_SANDBOX, E2E) or SYSTEM.match(entry) or ON_PATH.match(entry):
            continue
        if rootpath:
            if canonical(rootpath.group(1), package) not in data:
                found.append(f"{label}: {entry} names no executable of its `data`.")
            continue
        found.append(
            f"{label}: `{entry}` is no program: a system path under /bin, /sbin, /usr/bin, /usr/sbin or "
            f"/usr/libexec, a `$(rootpath <label>)` of its data, `path:<name>`, `{TEMPORARY}`, `{OWN_SANDBOX}` or `{E2E}`."
        )
    return found


def declared(tests):
    rows = []
    for label, test in sorted(tests.items()):
        package = label[2:].split(":")[0]
        for entry in entries_of(test["execs"]):
            rows.append((label, program_of(entry, package)))
    return rows


def read_reasons(root):
    reasons = {}
    for line in (Path(root) / REASONS).read_text().splitlines():
        if line.strip() and not line.startswith("#"):
            program, reason = line.split("\t", 1)
            reasons[program] = reason.strip()
    return reasons


def table(rows, reasons):
    by_program = collections.defaultdict(list)
    for label, program in rows:
        by_program[program].append(label)
    lines = [
        BEGIN,
        "| Program | Targets | Why |",
        "|---|---|---|",
    ]
    for program in sorted(by_program, key=lambda p: (-len(by_program[p]), p)):
        targets = ", ".join(f"`{label}`" for label in by_program[program])
        lines.append(f"| `{program}` | {targets} | {reasons.get(program, E2E_REASON if program == E2E else '')} |")
    for targets, reason in UNGUARDED.items():
        lines.append(f"| (none: not guarded) | {targets} | {reason} |")
    lines.append(END)
    return "\n".join(lines)


def section(text):
    start, end = text.find(BEGIN), text.find(END)
    if start < 0 or end < 0:
        return None
    return text[start:end + len(END)]


def check(tests, root):
    found = []
    for label, test in sorted(tests.items()):
        found += problems_of(label, test)
    rows = declared(tests)
    counted = [row for row in rows if row[1] not in (OWN_SANDBOX, E2E)]
    sandboxed = [row for row in rows if row[1] == OWN_SANDBOX]
    for name, rows_counted, ceiling in (
        ("EXEC_CEILING", counted, EXEC_CEILING),
        ("OWN_SANDBOX_CEILING", sandboxed, OWN_SANDBOX_CEILING),
    ):
        if len(rows_counted) > ceiling:
            found.append(
                f"{len(rows_counted)} entries against {name} = {ceiling}: a test may not start running another "
                "program; make it run inside the test, or give it a double."
            )
        elif len(rows_counted) < ceiling:
            found.append(f"{len(rows_counted)} entries: lower {name} in tools/test_execs.py to {len(rows_counted)}.")
    reasons = read_reasons(root)
    programs = {program for _, program in rows if program != E2E}
    for program in sorted(programs - set(reasons)):
        found.append(f"`{program}` has no reason in {REASONS}: say why a test runs it.")
    for program in sorted(set(reasons) - programs):
        found.append(f"{REASONS} gives a reason for `{program}`, which no target runs: remove the line.")
    current = section((Path(root) / DOC).read_text())
    if current != table(rows, reasons):
        found.append(f"The table in {DOC} is not the one the targets declare: `bazel run //tools:test_execs -- write`.")
    return found


def main(argv):
    command = argv[1]
    rules = argv[2] if len(argv) > 2 else RULES
    root = os.environ.get("BUILD_WORKSPACE_DIRECTORY", ".")
    tests = read_tests(rules)
    if command == "write":
        path = Path(root) / DOC
        text = path.read_text()
        path.write_text(text.replace(section(text), table(declared(tests), read_reasons(root))))
        return 0
    found = check(tests, root)
    for problem in found:
        print(problem, file=sys.stderr)
    return 1 if found else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
