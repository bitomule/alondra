#!/usr/bin/env python3
"""The state machine inventory, docs/architecture/state-machines.md, matches the code.

Every `### M-…` section of the inventory carries one `toml state-machine` block, and the inventory
holds one `toml state-files` block and one `toml timers` block. The guard checks, against the
production code of the crates (comments, literals' text and `#[cfg(test)]` items left out):

(a) every section has its block and every block its section;
(b) every symbol a block names (`path::Symbol`) is defined in that file;
(c) the variants a block declares are the variants of its enum;
(d) every state file name in production code is declared, with one writer while the daemon runs
    (and any `offline_writers` that only write it while the daemon is stopped, with `offline_why`),
    and is only named by the modules its declaration lists; every table a store creates is the truth of some machine;
    no crate writes with SQL to a table another crate's store creates;
(e) every time constant is in the timers block, saying whether it decides state or only paces;
(f) every enum named `*State`, `*Phase` or `*Status` is part of a machine.

Rules (e) and (f) read the domain and engine crates (`ENGINE`); the others read every scanned crate.

What is not state (a provider's own file, a test fixture name, an enum that only formats) is in
tools/state_inventory_exempt.tsv with its reason. What is wrong today and owned by an open fix is
in tools/state_inventory_known.tsv with its finding; an entry that no longer fails is a failure
too, so both lists only shrink."""
import importlib.util
import re
import sys
from pathlib import Path

INVENTORY = "docs/architecture/state-machines.md"
EXEMPT = "tools/state_inventory_exempt.tsv"
KNOWN = "tools/state_inventory_known.tsv"

NOT_SCANNED = {"e2e", "engine-doubles", "machine-model", "provider-conformance", "scenarios", "scenarios-transcript"}
ENGINE = re.compile(r"^(?:domain|domain-rules|core|engine-[\w-]+|daemon-[\w-]+|alondrad|store|store-sqlite|agent-[\w-]+|provider-[\w-]+|host|system)$")
STATE_FILE = re.compile(r"^(?:[\w-]+/)*[\w-][\w.-]*\.(?:json|toml|pending)$")
SECTION = re.compile(r"^### (M-[A-Z]+-[A-Z0-9-]+) ·", re.M)
FENCE = re.compile(r"^```toml (state-machine|state-files|timers)\n(.*?)^```$", re.M | re.S)
ENUM = re.compile(r"\benum\s+(\w*(?:State|Phase|Status))\b")
DURATION_CONST = re.compile(
    r"\b(?:const|static)\s+([A-Z][A-Z0-9_]*)\s*:\s*&?(?:\[\s*)?(?:(?:std|core|alondra_core|alondra_foundation|crate)::(?:time::)?)?(?:Duration|Millis)\b")
MILLIS_CONST = re.compile(
    r"\b(?:const|static)\s+([A-Z][A-Z0-9_]*_(?:MS|MILLIS|SECS|SECONDS|MINUTES|MINS|HOURS))\s*:\s*[iu](?:8|16|32|64|128|size)\b")
SQL_WRITE = re.compile(r"\b(?:INSERT(?:\s+OR\s+[A-Z]+)?\s+INTO|REPLACE\s+INTO|UPDATE|DELETE\s+FROM)\s+([a-z_][a-z0-9_]*)\b")
CREATE_TABLE = re.compile(r"\bCREATE\s+TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?([a-z_][a-z0-9_]*)\b")
SQLITE_INTERNAL = {"sqlite_schema", "sqlite_master", "sqlite_sequence", "main", "temp"}
DEFINITION = {
    ".rs": r"\b(?:enum|struct|fn|trait|type|const|static|mod|union|macro_rules!)\s+{}\b",
    ".ts": r"\b(?:function|class|const|let|type|interface|enum)\s+{}\b",
    ".swift": r"\b(?:class|struct|enum|func|actor|protocol|typealias|var|let|extension)\s+{}\b",
    ".sql": r"\bTABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?{}\b",
}

spec = importlib.util.spec_from_file_location("clock_guard", Path(__file__).resolve().parent / "clock_guard.py")
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)


class BlockError(ValueError):
    pass


VALUE = re.compile(r'"((?:\\.|[^"\\])*)"|(true|false)\b')


def parse_block(body):
    """The blocks' TOML subset: `key = "text"`, `key = ["text", ...]`, `key = true|false` and `[[table]]`
    headers. The system Python Bazel runs the test with has no tomllib."""
    data, current = {}, None
    for number, line in enumerate(body.splitlines(), 1):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        header = re.fullmatch(r"\[\[(\w+)\]\]", line)
        if header:
            current = {}
            data.setdefault(header.group(1), []).append(current)
            continue
        pair = re.fullmatch(r"(\w+)\s*=\s*(.+)", line)
        if not pair:
            raise BlockError(f"line {number}: not key = value")
        key, raw = pair.groups()
        if raw.startswith("["):
            if not raw.endswith("]"):
                raise BlockError(f"line {number}: a list must close on its line")
            inner = raw[1:-1].strip()
            items = [m.group(1) for m in VALUE.finditer(inner)]
            if re.sub(r'"(?:\\.|[^"\\])*"|[\s,]', "", inner):
                raise BlockError(f"line {number}: a list holds only strings")
            value = [item.replace('\\"', '"').replace("\\\\", "\\") for item in items]
        else:
            match = VALUE.fullmatch(raw)
            if not match:
                raise BlockError(f"line {number}: a value is a string, a list of strings, true or false")
            value = match.group(2) == "true" if match.group(2) else match.group(1).replace('\\"', '"').replace("\\\\", "\\")
        (current if current is not None else data)[key] = value
    return data


def line_of(text, offset):
    return text.count("\n", 0, offset) + 1


def literals(text, code):
    """Every string literal of production code: (offset of its opening quote, its text)."""
    found = []
    for match in re.finditer(r'(?<![\w])(b?r(#*)"(.*?)"\2)|(?<![\w\'])b?"((?:\\.|[^"\\])*)"', text, re.S):
        quote = match.start() + match.group(0).index('"')
        if code[quote] != '"':
            continue
        found.append((quote, match.group(3) if match.group(1) else match.group(4)))
    return found


def production_files(root):
    """Production Rust files under `src/` of every scanned crate, the way the clock guard finds them."""
    for crate in sorted(p for p in (root / "crates").iterdir() if p.is_dir() and p.name not in NOT_SCANNED):
        base = guard.absolute(crate)
        files = sorted(guard.absolute(path) for path in (base / "src").rglob("*.rs") if "fixtures" not in path.parts)
        test_only = {path for path in files if guard.is_test_file(path.relative_to(base))}
        changed = True
        while changed:
            changed = False
            for path in files:
                for module, marked in guard.declared_modules(path, path.read_text()):
                    module = guard.absolute(module)
                    if (marked or path in test_only) and module.exists() and module not in test_only:
                        test_only.add(module)
                        changed = True
        for path in files:
            if path not in test_only:
                yield path


class Source:
    def __init__(self, root, path):
        self.path = path.relative_to(root).as_posix()
        self.crate = self.path.split("/")[1]
        self.text = path.read_text()
        self.code = guard.production_code(self.text)


def read_tsv(path, columns):
    rows = []
    if not path.exists():
        return rows
    for number, line in enumerate(path.read_text().splitlines(), 1):
        if not line.strip() or line.startswith("#"):
            continue
        cells = line.split("\t")
        if len(cells) != columns or not all(cell.strip() for cell in cells):
            raise SystemExit(f"{path}:{number}: expected {columns} non-empty tab-separated columns")
        rows.append(tuple(cell.strip() for cell in cells))
    return rows


class Inventory:
    def __init__(self, text):
        self.text = text
        self.sections = {}
        for match in SECTION.finditer(text):
            if match.group(1) in self.sections:
                raise SystemExit(f"{INVENTORY}:{line_of(text, match.start())}: section {match.group(1)} twice")
            self.sections[match.group(1)] = match.start()
        self.machines, self.files, self.timers, self.problems = {}, {}, {}, []
        for fence in FENCE.finditer(text):
            kind, body, at = fence.group(1), fence.group(2), line_of(text, fence.start())
            try:
                data = parse_block(body)
            except BlockError as error:
                self.problems.append(f"{INVENTORY}:{at}: unreadable {kind} block: {error}")
                continue
            if kind == "state-machine":
                self.machine(data, fence.start(), at)
            elif kind == "state-files":
                for entry in data.get("file", []):
                    self.files[entry.get("name", "")] = (entry, at)
            else:
                for entry in data.get("timer", []):
                    self.timers[(entry.get("where", ""), entry.get("name", ""))] = (entry, at)

    def machine(self, data, offset, at):
        identifier = data.get("id")
        owner = None
        for name, start in self.sections.items():
            if start < offset and (owner is None or start > self.sections[owner]):
                owner = name
        if identifier != owner:
            self.problems.append(f"{INVENTORY}:{at}: block id {identifier!r} sits in section {owner}")
        if identifier in self.machines:
            self.problems.append(f"{INVENTORY}:{at}: second block for {identifier}")
        self.machines[identifier] = (data, at)


def defined(root, reference):
    """Whether `path::Symbol` (or a bare path) names something that exists."""
    path, _, symbol = reference.partition("::")
    target = root / path
    if not target.is_file():
        return False
    if not symbol:
        return True
    pattern = DEFINITION.get(target.suffix)
    return pattern is None or re.search(pattern.format(re.escape(symbol)), guard.blank(target.read_text())) is not None


def enum_variants(root, reference):
    path, _, name = reference.partition("::")
    target = root / path
    if not target.is_file():
        return None
    code = guard.blank(target.read_text())
    match = re.search(rf"\benum\s+{re.escape(name)}\b[^{{;]*\{{", code)
    if not match:
        return None
    body = code[match.end(): guard.closing(code, match.end() - 1) - 1]
    while (attribute := re.search(r"#\s*\[", body)) is not None:
        body = body[:attribute.start()] + body[guard.closing(body, attribute.end() - 1):]
    variants, depth, current = [], 0, ""
    for c in body + ",":
        if c in "([{<":
            depth += 1
        elif c in ")]}>":
            depth -= 1
        if c == "," and depth == 0:
            found = re.match(r"\s*([A-Za-z_]\w*)", current)
            if found:
                variants.append(found.group(1))
            current = ""
        else:
            current += c
    return variants


def check(root, inventory=INVENTORY, exempt=EXEMPT, known=KNOWN):
    root = guard.absolute(root)
    inventory = Inventory((root / inventory).read_text())
    exempt = {(kind, name): reason for kind, name, reason in read_tsv(root / exempt, 3)}
    known = {(rule, subject): finding for rule, subject, finding in read_tsv(root / known, 3)}
    found = list(inventory.problems)
    failures = {}

    def fail(rule, subject, message):
        failures.setdefault((rule, subject), []).append(message)

    for identifier, offset in inventory.sections.items():
        if identifier not in inventory.machines:
            fail("section", identifier, f"{INVENTORY}:{line_of(inventory.text, offset)}: {identifier} has no state-machine block")

    referenced = set()
    truths = set()
    for identifier, (data, at) in inventory.machines.items():
        code = data.get("code", [])
        if not code and not data.get("removed"):
            fail("symbol", identifier, f"{INVENTORY}:{at}: {identifier} names no code")
        for reference in code:
            referenced.add(reference)
            if not defined(root, reference):
                fail("symbol", reference, f"{INVENTORY}:{at}: {identifier}: {reference} is not defined")
        truths.update(t.split(":", 1)[1] for t in data.get("truth", []) if t.startswith(("sqlite:", "postgres:")))
        states = data.get("states")
        if states:
            referenced.add(states)
            declared = data.get("variants", [])
            real = enum_variants(root, states)
            if real is None:
                fail("symbol", states, f"{INVENTORY}:{at}: {identifier}: enum {states} not found")
            elif sorted(real) != sorted(declared):
                missing = sorted(set(real) - set(declared))
                extra = sorted(set(declared) - set(real))
                fail("states", states, f"{INVENTORY}:{at}: {identifier}: {states} has {missing} undeclared and {extra} declared but gone")
            for terminal in data.get("terminals", []):
                if terminal not in declared:
                    fail("states", states, f"{INVENTORY}:{at}: {identifier}: terminal {terminal} is not a declared variant")

    sources = [Source(root, path) for path in production_files(root)]
    named = {}
    created = {}
    for source in sources:
        for quote, value in literals(source.text, source.code):
            if STATE_FILE.match(value):
                named.setdefault(value, set()).add(source.path)
            for table in CREATE_TABLE.findall(value):
                created.setdefault(table, set()).add(source.crate)
    for path in sorted((root / "crates").glob("*/**/*.sql")):
        relative = path.relative_to(root).as_posix()
        if "fixtures" in path.parts or relative.split("/")[1] in NOT_SCANNED:
            continue
        crate = relative.split("/")[1]
        for table in CREATE_TABLE.findall(path.read_text()):
            created.setdefault(table, set()).add("store-sqlite" if crate == "domain-store-schema" else crate)

    for name, modules in sorted(named.items()):
        if ("file", name) in exempt:
            continue
        if name not in inventory.files:
            fail("file", name, f"{sorted(modules)[0]}: state file {name} is not in the state-files block")
            continue
        entry, at = inventory.files[name]
        writer = entry.get("writer", [])
        writer = [writer] if isinstance(writer, str) else writer
        if len(writer) != 1:
            fail("writer", name, f"{INVENTORY}:{at}: {name} has {len(writer)} writers {writer}; a state file has one")
        offline = entry.get("offline_writers", [])
        if offline and not entry.get("offline_why"):
            fail("writer", name, f"{INVENTORY}:{at}: {name} has offline writers without offline_why")
        allowed = set(writer) | set(entry.get("readers", [])) | set(offline)
        for module in sorted(modules - allowed):
            fail("owner", f"{name} {module}", f"{module}: names {name}, whose declaration lists only {sorted(allowed)}")
        if entry.get("machine") not in inventory.machines:
            fail("file", name, f"{INVENTORY}:{at}: {name} belongs to no known machine ({entry.get('machine')!r})")
    for name, (entry, at) in sorted(inventory.files.items()):
        if name not in named:
            fail("file", name, f"{INVENTORY}:{at}: {name} is declared but no production code names it")

    for table in sorted(created):
        if table not in truths and ("table", table) not in exempt:
            fail("table", table, f"{sorted(created[table])[0]}: table {table} is the truth of no machine")
    for source in sources:
        for quote, value in literals(source.text, source.code):
            for match in SQL_WRITE.finditer(value):
                table = match.group(1)
                owners = created.get(table)
                if table in SQLITE_INTERNAL or owners is None or source.crate in owners:
                    continue
                fail("sql-write", f"{table} {source.path}",
                     f"{source.path}:{line_of(source.text, quote)}: writes table {table}, which {sorted(owners)} owns")

    seen_timers = set()
    for source in (s for s in sources if ENGINE.match(s.crate)):
        for pattern in (DURATION_CONST, MILLIS_CONST):
            for match in pattern.finditer(source.code):
                key = (source.path, match.group(1))
                seen_timers.add(key)
                if key not in inventory.timers and ("timer", f"{source.path}::{match.group(1)}") not in exempt:
                    fail("timer", f"{source.path}::{match.group(1)}",
                         f"{source.path}:{line_of(source.text, match.start())}: time constant {match.group(1)} is not in the timers block")
    for key, (entry, at) in sorted(inventory.timers.items()):
        if key not in seen_timers:
            fail("timer", f"{key[0]}::{key[1]}", f"{INVENTORY}:{at}: timer {key[1]} is declared in {key[0]} but not there")
        if entry.get("decides") not in ("state", "pace"):
            fail("timer", f"{key[0]}::{key[1]}", f"{INVENTORY}:{at}: timer {key[1]} needs decides = \"state\" or \"pace\"")
        if entry.get("decides") == "state" and entry.get("machine") not in inventory.machines:
            fail("timer", f"{key[0]}::{key[1]}", f"{INVENTORY}:{at}: timer {key[1]} decides state of no known machine")

    for source in (s for s in sources if ENGINE.match(s.crate)):
        for match in ENUM.finditer(source.code):
            reference = f"{source.path}::{match.group(1)}"
            if reference not in referenced and ("enum", reference) not in exempt:
                fail("enum", reference, f"{source.path}:{line_of(source.text, match.start())}: enum {match.group(1)} is part of no machine")

    for key, messages in sorted(failures.items()):
        if key not in known:
            found.extend(messages)
    for (rule, subject), finding in sorted(known.items()):
        if (rule, subject) not in failures:
            found.append(f"{KNOWN}: {rule} {subject} ({finding}) no longer fails: remove it")
    return found


if __name__ == "__main__":
    arguments = sys.argv[1:]
    found = check(Path(arguments[0] if arguments else "."), *arguments[1:4])
    print("\n".join(found))
    sys.exit(1 if found else 0)
