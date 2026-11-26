#!/usr/bin/env python3
"""The public surface of a Rust crate, read from its sources.

`surface(source_dir)` lists every public item of the `.rs` files under a folder, one per line and
sorted: functions with the types of their parameters and what they return, structs with their
public fields, enums with their variants, traits with their methods, constants, type aliases, the
public methods of every `impl` and what `pub use` exports from another crate. The text of the list is the same however the code is laid out
in files, modules and lines, so a snapshot of it says whether a crate that was written again kept
its surface.

An item is listed when code outside the crate can name it: it says `pub` and every module around
it does, or a `pub use` in a module that can be reached names it, or its module, or everything in
its module. The methods and the trait implementations of a type are listed only when the type is.

It reads the text, not the compiler's view: what a macro generates is not in the list, and names
are compared without their paths, so an item that is not exported is still listed when an exported
one of another module has its name.

    python3 tools/public_surface.py crates/spec-engine/src > specs/spec-engine/surface.txt
"""

import re
import sys
from pathlib import Path

OPENERS = {"(": ")", "[": "]", "{": "}", "<": ">"}
QUALIFIERS = r"(?:(?:const|async|unsafe|extern)\s+)*"
NAME_AND_TYPE = re.compile(r"(?<!:):(?!:)")
TYPE = re.compile(r"(?:pub(?:\([^)]*\))?\s+)?(?:unsafe\s+)?(?:struct|enum|union|trait|type)\s+(\w+)")


def production_files(source_dir):
    for path in sorted(source_dir.rglob("*.rs")):
        relative = path.relative_to(source_dir)
        if path.name == "tests.rs" or path.name.endswith("_tests.rs") or "tests" in relative.parts[:-1]:
            continue
        yield path


def without_comments_and_literals(source):
    out = []
    index = 0
    while index < len(source):
        char = source[index]
        pair = source[index : index + 2]
        if pair == "//":
            end = source.find("\n", index)
            index = len(source) if end == -1 else end
        elif pair == "/*":
            depth = 1
            index += 2
            while index < len(source) and depth:
                if source[index : index + 2] == "/*":
                    depth += 1
                    index += 2
                elif source[index : index + 2] == "*/":
                    depth -= 1
                    index += 2
                else:
                    index += 1
            out.append(" ")
        elif char == '"':
            index += 1
            while index < len(source) and source[index] != '"':
                index += 2 if source[index] == "\\" else 1
            index += 1
            out.append('""')
        elif char == "r" and re.match(r'r#*"', source[index:]):
            hashes = len(re.match(r"r(#*)", source[index:]).group(1))
            end = source.find('"' + "#" * hashes, index + hashes + 2)
            index = len(source) if end == -1 else end + hashes + 1
            out.append('""')
        elif char == "'" and re.match(r"'(\\.[^']*|[^'\\])'", source[index:]):
            index += len(re.match(r"'(\\.[^']*|[^'\\])'", source[index:]).group(0))
            out.append("' '")
        else:
            out.append(char)
            index += 1
    return "".join(out)


def without_attributes(source):
    out = []
    index = 0
    while index < len(source):
        if source[index] == "#" and re.match(r"#!?\[", source[index:]):
            index = source.index("[", index)
            depth = 0
            while index < len(source):
                depth += {"[": 1, "]": -1}.get(source[index], 0)
                index += 1
                if depth == 0:
                    break
        else:
            out.append(source[index])
            index += 1
    return "".join(out)


def split_top(source, separator):
    """The pieces of a text between separators that are outside every bracket."""
    pieces = []
    depth = 0
    start = 0
    for index, char in enumerate(source):
        if char in "([{":
            depth += 1
        elif char in ")]}":
            depth -= 1
        elif char == "<":
            depth += 1
        elif char == ">" and source[index - 1 : index] != "-":
            depth -= 1
        elif char == separator and depth == 0:
            pieces.append(source[start:index])
            start = index + 1
    pieces.append(source[start:])
    return [piece.strip() for piece in pieces if piece.strip()]


def items(source):
    """Each item of a block as (header, body): the body is the text between its braces, or None."""
    found = []
    index = 0
    start = 0
    depth = 0
    while index < len(source):
        char = source[index]
        if char in "([":
            depth += 1
        elif char in ")]":
            depth -= 1
        elif char == ";" and depth == 0:
            found.append((source[start:index].strip(), None))
            start = index + 1
        elif char == "{" and depth == 0:
            end = closing(source, index)
            found.append((source[start:index].strip(), source[index + 1 : end]))
            index = end
            start = end + 1
        index += 1
    return [(header, body) for header, body in found if header]


def closing(source, opening):
    depth = 0
    for index in range(opening, len(source)):
        depth += {"{": 1, "}": -1}.get(source[index], 0)
        if depth == 0:
            return index
    return len(source) - 1


def tidy(text):
    text = re.sub(r"\s+", " ", text).strip()
    text = re.sub(r"\s*([<>(),\[\]&:;=])\s*", r"\1", text)
    text = text.replace("-", " -").replace("->", "-> ").replace("  ", " ")
    text = re.sub(r",(?=\S)", ", ", text)
    text = re.sub(r"(?<!:):(?!:)(?=\S)", ": ", text)
    return text.replace("=", " = ").strip()


def signature(header):
    match = re.match(r"(?:pub\s+)?" + QUALIFIERS + r"fn\s+(\w+)\s*(<.*?>)?\s*\(", header, re.S)
    if not match:
        return None
    opening = match.end() - 1
    depth = 0
    for index in range(opening, len(header)):
        depth += {"(": 1, ")": -1}.get(header[index], 0)
        if depth == 0:
            break
    parameters = []
    for parameter in split_top(header[opening + 1 : index], ","):
        if re.fullmatch(r"(?:&\s*(?:'\w+\s+)?)?(?:mut\s+)?self", parameter):
            parameters.append(tidy(parameter))
        else:
            parameters.append(tidy(NAME_AND_TYPE.split(parameter, maxsplit=1)[-1]))
    rest = tidy(header[index + 1 :])
    return f"fn {match.group(1)}{tidy(match.group(2) or '')}({', '.join(parameters)})" + (f" {rest}" if rest else "")


def exported(tree):
    """Each path of a `use` tree, with its groups written out."""
    opening = tree.find("{")
    if opening < 0:
        return [tidy(tree)]
    end = closing(tree, opening)
    return [path for piece in split_top(tree[opening + 1 : end], ",") if piece.strip() for path in exported(tree[:opening] + piece.strip() + tree[end + 1 :])]


class Crate:
    """What the sources of a crate declare, before deciding what of it can be reached from outside."""

    def __init__(self):
        self.entries = []
        self.uses = []
        self.types = set()
        self.modules = {}


def type_name(text):
    """The name of a type without its references, path and parameters."""
    match = re.match(r"(?:\w+::)*(\w+)", re.sub(r"^(?:&\s*|mut\s+|dyn\s+)+", "", text))
    return match.group(1) if match else text


def read_block(source, module, crate, owner=None):
    for header, body in items(source):
        public = re.match(r"pub\s+(?!\()", header) is not None
        inner = re.match(r"(pub(?:\([^)]*\))?\s+)?mod\s+(\w+)$", header)
        implementation = re.match(r"(?:unsafe\s+)?impl\b\s*(<.*?>)?\s*(.*)$", header, re.S)
        declared = None if owner else TYPE.match(header)
        if declared:
            crate.types.add(declared.group(1))
        if inner:
            child = module + (inner.group(2),)
            crate.modules[child] = inner.group(1) is not None and "(" not in inner.group(1)
            if body is not None:
                read_block(body, child, crate)
        elif public and re.match(r"pub\s+use\b", header):
            tree = re.sub(r"^pub\s+use\s+", "", header) + ("" if body is None else "{" + body + "}")
            crate.uses += [(path, module) for path in exported(tree)]
        elif implementation and body is not None:
            target = tidy(implementation.group(2))
            if " for " in target:
                crate.entries.append(("of", f"impl {target}", type_name(target.split(" for ", 1)[1]), module))
            else:
                read_block(body, module, crate, owner=target)
        elif public and re.match(r"pub\s+" + QUALIFIERS + r"fn\b", header):
            line = signature(header)
            if owner:
                crate.entries.append(("of", f"impl {owner}: {line}", type_name(owner), module))
            else:
                crate.entries.append(("item", line, re.match(r"fn (\w+)", line).group(1), module))
        elif public and (match := re.match(r"pub\s+struct\s+(\w+)\s*(<.*?>)?", header, re.S)):
            name = match.group(1) + tidy(match.group(2) or "")
            if body is None:
                line = f"struct {name}{tidy(header[match.end():])}"
            else:
                fields = [tidy(re.sub(r"^pub\s+", "", field)) for field in split_top(body, ",") if re.match(r"pub\s+(?!\()", field)]
                line = f"struct {name} {{ {', '.join(fields)} }}" if fields else f"struct {name} {{ }}"
            crate.entries.append(("type", line, match.group(1), module))
        elif public and (match := re.match(r"pub\s+enum\s+(\w+)", header)):
            variants = [re.match(r"\w+", variant).group(0) for variant in split_top(body or "", ",")]
            crate.entries.append(("type", f"enum {match.group(1)} {{ {', '.join(variants)} }}", match.group(1), module))
        elif public and (match := re.match(r"pub\s+(?:unsafe\s+)?trait\s+(\w+)", header)):
            methods = [signature(method) for method, _ in items(body or "")]
            crate.entries.append(("type", f"trait {match.group(1)} {{ {'; '.join(method for method in methods if method)} }}", match.group(1), module))
        elif public and (match := re.match(r"pub\s+(const|static|type)\s+(?:mut\s+)?(\w+)", header)):
            line = tidy(re.sub(r"^pub\s+", "", header.split("=")[0]))
            if owner:
                crate.entries.append(("of", line, type_name(owner), module))
            else:
                crate.entries.append(("type" if match.group(1) == "type" else "item", line, match.group(2), module))


def module_of(relative):
    parts = relative.parts[:-1]
    return parts if relative.stem in ("lib", "main", "mod") else parts + (relative.stem,)


def exports(crate, inside):
    """The names and the modules that a `pub use` of a module that can be reached hands outside."""
    names, modules = set(), set()
    while True:
        found = (set(), set())
        for path, module in crate.uses:
            segments = path.split(" as ")[0].split("::")
            if segments[0] not in inside or not reachable(crate, module, modules):
                continue
            everything = segments[-1] == "*"
            target = resolved(segments[:-1] if everything else segments, module)
            if everything or target in crate.modules:
                found[1].add(target)
            else:
                found[0].add(segments[-1])
        if found == (names, modules):
            return names, modules
        names, modules = found


def resolved(segments, module):
    """The module a path names, written from the root of the crate."""
    if segments[0] == "crate":
        return tuple(segments[1:])
    while segments and segments[0] in ("self", "super"):
        module = module[:-1] if segments[0] == "super" else module
        segments = segments[1:]
    return tuple(module) + tuple(segments)


def reachable(crate, module, exported_modules):
    if not module or module in exported_modules:
        return True
    return crate.modules.get(module, True) and reachable(crate, module[:-1], exported_modules)


def surface(source_dir):
    crate = Crate()
    inside = {"crate", "self", "super"}
    for path in production_files(Path(source_dir)):
        source = without_attributes(without_comments_and_literals(path.read_text()))
        inside.update(re.findall(r"\bmod\s+(\w+)", source))
        read_block(source, module_of(path.relative_to(source_dir)), crate)
    names, modules = exports(crate, inside)

    def named(name, module):
        return name in names or reachable(crate, module, modules)

    public_types = {name for kind, _, name, module in crate.entries if kind == "type" and named(name, module)}
    lines = set()
    for kind, line, name, module in crate.entries:
        of_a_listed_type = name in public_types or name not in crate.types
        if of_a_listed_type if kind == "of" else named(name, module):
            lines.add(line)
    for path, module in crate.uses:
        if path.split("::")[0] not in inside and named(path.split(" as ")[-1].split("::")[-1], module):
            lines.add(f"use {path}")
    return "".join(f"{line}\n" for line in sorted(lines))


def main():
    if len(sys.argv) != 2:
        sys.exit("usage: public_surface.py <source folder>")
    sys.stdout.write(surface(Path(sys.argv[1])))


if __name__ == "__main__":
    main()
