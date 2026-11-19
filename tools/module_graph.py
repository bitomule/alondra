#!/usr/bin/env python3
"""Maps the modules of a crate: lines, tests, the modules each one uses, and the groups of modules
that depend on each other in a cycle. A cycle has to be cut before its modules can live in separate
crates. Reads sources only; nothing is built.

    python3 tools/module_graph.py crates/daemon-engine/src
    python3 tools/module_graph.py --production crates/daemon-engine/src

`--production` counts only what the library compiles outside its tests: the files `lib.rs` reaches
through its `mod` declarations, without comments, the items only tests compile, or the fixtures a crate
compiles for the tests of the crates above it (modules named `fixtures`, `testing`, `fake` or
`*_fixtures`), which no production module reaches. That is the graph a crate
split has to cut."""
import collections
import os
import re
import sys
from pathlib import Path

TEST_ONLY = re.compile(r"#\[cfg\(test\)\]")
MOD = re.compile(r"((?:#\[[^\]]*\]\s*)*)(?:pub(?:\([^)]*\))?\s+)?mod\s+([a-z_0-9]+)\s*;")
PATH = re.compile(r'#\[path\s*=\s*"([^"]+)"\]')
FIXTURES = re.compile(r"(?:fixtures|testing|fake|[a-z_0-9]+_fixtures)")
INLINE_FIXTURES = re.compile(r"(?:pub(?:\([^)]*\))?\s+)?mod\s+(?:fixtures|testing|fake|[a-z_0-9]+_fixtures)\s*\{")


def modules(root):
    found = {}
    for entry in sorted(root.iterdir()):
        if entry.name == "lib.rs":
            continue
        if entry.is_dir():
            name, files = entry.name, list(entry.rglob("*.rs"))
        elif entry.suffix == ".rs":
            name, files = entry.stem.removesuffix("_tests"), [entry]
        else:
            continue
        module = found.setdefault(name, {"lines": 0, "tests": 0, "files": []})
        for file in files:
            text = file.read_text()
            module["files"].append(text)
            module["lines"] += text.count("\n")
            module["tests"] += len(re.findall(r"#\[(?:tokio::)?test", text))
    return found


def skip_literal(text, at):
    """Where the string, char literal or comment starting at `at` ends; `at` when none starts there."""
    if text.startswith("//", at):
        end = text.find("\n", at)
        return len(text) if end < 0 else end
    if text.startswith("/*", at):
        depth, index = 0, at
        while index < len(text):
            if text.startswith("/*", index):
                depth, index = depth + 1, index + 2
            elif text.startswith("*/", index):
                depth, index = depth - 1, index + 2
                if depth == 0:
                    return index
            else:
                index += 1
        return len(text)
    raw = re.match(r'b?r(#*)"', text[at:at + 260])
    if raw and (at == 0 or not (text[at - 1].isalnum() or text[at - 1] == "_")):
        closing = '"' + raw.group(1)
        end = text.find(closing, at + raw.end())
        return len(text) if end < 0 else end + len(closing)
    if text[at] == '"':
        index = at + 1
        while index < len(text) and text[index] != '"':
            index += 2 if text[index] == "\\" else 1
        return index + 1
    if text[at] == "'":
        char = re.match(r"'(?:\\(?:u\{[0-9a-fA-F]+\}|x[0-9a-fA-F]{2}|.)|[^\\'])'", text[at:at + 12])
        return at + char.end() if char else at
    return at


def blank(piece):
    return re.sub(r"[^\n]", " ", piece)


def code_only(text):
    """`text` with its comments and literals blanked, so braces and paths in them never count; every
    other character stays at its offset."""
    out, index = [], 0
    while index < len(text):
        end = skip_literal(text, index)
        if end == index:
            out.append(text[index])
            index += 1
            continue
        out.append(blank(text[index:end]))
        index = end
    return "".join(out)


ITEM = re.compile(r"\s*(?:#\[[^\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?(?:(?:async|unsafe|const|extern)\s+)*(?:fn|impl|mod|struct|enum|trait|use|static|const|type|macro_rules!|union)\b")
OPENING, CLOSING = "{([", "})]"


def item_end(code, at):
    """Where what the attribute at `at` marks ends. An item ends at its `;` outside braces or at the
    brace closing its body; a field, variant, match arm or statement also ends at a `,` outside any
    bracket, or where the brackets around it close."""
    item = ITEM.match(code, at) is not None
    depth = angles = 0
    for index in range(at, len(code)):
        char = code[index]
        if char in OPENING:
            depth += 1
        elif char in CLOSING:
            depth -= 1
            if depth < 0:
                return index
            if depth == 0 and char == "}" and (item or code[index + 1:].lstrip()[:1] != ","):
                return index + 1
        elif char == "<" and not item:
            angles += 1
        elif char == ">" and not item and code[index - 1] not in "-=":
            angles = max(angles - 1, 0)
        elif char == ";" and depth == 0:
            return index + 1
        elif char == "," and depth == 0 and angles == 0 and not item:
            return index + 1
    return len(code)


def without_tests(code, text):
    """`code` with the items under `#[cfg(test)]` blanked, attributes after it included. `text` is the
    source `code` blanked its literals of."""
    out, index = [], 0
    for found in TEST_ONLY.finditer(text):
        start = found.start()
        if start < index or not code.startswith("#[cfg(", start):
            continue
        out.append(code[index:start])
        index = item_end(code, found.end())
        out.append(blank(code[start:index]))
    out.append(code[index:])
    return "".join(out)


def child_file(parent, name, path):
    """The file of `mod name;` declared in `parent`, as rustc looks for it."""
    owns_directory = parent.name in ("lib.rs", "mod.rs")
    directory = parent.parent if owns_directory else parent.parent / parent.stem
    if path:
        return parent.parent / path
    for candidate in (directory / f"{name}.rs", directory / name / "mod.rs"):
        if candidate.exists():
            return candidate
    return None


def without_fixtures(code):
    """`code` with its inline fixture modules blanked."""
    out, index = [], 0
    for found in INLINE_FIXTURES.finditer(code):
        if found.start() < index:
            continue
        out.append(code[index:found.start()])
        index = item_end(code, found.start())
        out.append(blank(code[found.start():index]))
    out.append(code[index:])
    return "".join(out)


def production_modules(root):
    """Each module `lib.rs` declares, with the production code of every file it reaches."""
    found = {}

    def visit(file, module):
        text = file.read_text()
        code = without_fixtures(without_tests(code_only(text), text))
        if module is not None:
            found[module]["files"].append(code)
            found[module]["lines"] += sum(1 for line in code.splitlines() if line.strip())
        for declared in MOD.finditer(code):
            name = declared.group(2)
            if FIXTURES.fullmatch(name):
                continue
            path = PATH.search(text[declared.start(1):declared.end(1)])
            child = child_file(file, name, path.group(1) if path else None)
            if child is None or not Path(os.path.normpath(child)).is_relative_to(os.path.normpath(root)):
                continue
            top = module if module is not None else name
            found.setdefault(top, {"lines": 0, "tests": 0, "files": []})
            visit(child, top)

    visit(root / "lib.rs", None)
    return found


def dependencies(found):
    for name, module in found.items():
        uses = collections.Counter()
        for text in module["files"]:
            named = re.findall(r"crate::([a-z_0-9]+)", text)
            for group in re.findall(r"crate::\{([^}]*)\}", text):
                named += re.findall(r"\b([a-z_0-9]+)\b", group)
            uses.update(use for use in named if use in found and use != name)
        module["uses"] = uses


def cycles(found):
    index, low, stack, on_stack, groups = {}, {}, [], set(), []

    def visit(node):
        index[node] = low[node] = len(index)
        stack.append(node)
        on_stack.add(node)
        for other in found[node]["uses"]:
            if other not in index:
                visit(other)
                low[node] = min(low[node], low[other])
            elif other in on_stack:
                low[node] = min(low[node], index[other])
        if low[node] == index[node]:
            group = []
            while True:
                member = stack.pop()
                on_stack.discard(member)
                group.append(member)
                if member == node:
                    break
            groups.append(sorted(group))

    sys.setrecursionlimit(10000)
    for node in found:
        if node not in index:
            visit(node)
    return sorted(groups, key=len, reverse=True)


def main_cycle(root):
    """The largest group of production modules that depend on each other, with its lines."""
    found = production_modules(Path(root))
    dependencies(found)
    group = cycles(found)[0] if found else []
    if len(group) < 2:
        return [], 0
    return group, sum(found[name]["lines"] for name in group)


def main(argv):
    production = "--production" in argv
    root = Path([arg for arg in argv if arg != "--production"][0])
    found = production_modules(root) if production else modules(root)
    dependencies(found)
    for name, module in sorted(found.items()):
        uses = " ".join(f"{use}:{count}" for use, count in sorted(module["uses"].items()))
        print(f"{name} lines={module['lines']} tests={module['tests']} uses {uses}")
    print(f"total lines={sum(m['lines'] for m in found.values())} tests={sum(m['tests'] for m in found.values())}")
    for group in cycles(found):
        if len(group) > 1:
            lines = sum(found[m]["lines"] for m in group)
            tests = sum(found[m]["tests"] for m in group)
            print(f"cycle of {len(group)} modules, {lines} lines, {tests} tests: {' '.join(group)}")


if __name__ == "__main__":
    main(sys.argv[1:])
