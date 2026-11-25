#!/usr/bin/env python3
"""The public surface of a Rust crate, read from its sources.

`surface(source_dir)` lists every public item of the `.rs` files under a folder, one per line and
sorted: functions with the types of their parameters and what they return, structs with their
public fields, enums with their variants, traits with their methods, constants, type aliases, the
public methods of every `impl` and what `pub use` exports from another crate. The text of the list is the same however the code is laid out
in files, modules and lines, so a snapshot of it says whether a crate that was written again kept
its surface.

It reads the text, not the compiler's view: what a macro generates is not in the list, and an item
is listed when it says `pub` even if the module around it is private.

    python3 tools/public_surface.py crates/spec-engine/src > specs/spec-engine/surface.txt
"""

import re
import sys
from pathlib import Path

OPENERS = {"(": ")", "[": "]", "{": "}", "<": ">"}
QUALIFIERS = r"(?:(?:const|async|unsafe|extern)\s+)*"
NAME_AND_TYPE = re.compile(r"(?<!:):(?!:)")


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


def surface_of(source, owner=None):
    lines = []
    for header, body in items(source):
        public = re.match(r"pub\s+(?!\()", header) is not None
        module = re.match(r"(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+$", header)
        implementation = re.match(r"(?:unsafe\s+)?impl\b\s*(<.*?>)?\s*(.*)$", header, re.S)
        if module and body is not None:
            lines += surface_of(body)
        elif public and re.match(r"pub\s+use\b", header):
            tree = re.sub(r"^pub\s+use\s+", "", header) + ("" if body is None else "{" + body + "}")
            lines += [f"use {path}" for path in exported(tree)]
        elif implementation and body is not None:
            target = tidy(implementation.group(2))
            if " for " in target:
                lines.append(f"impl {target}")
            else:
                lines += surface_of(body, owner=target)
        elif public and re.match(r"pub\s+" + QUALIFIERS + r"fn\b", header):
            line = signature(header)
            lines.append(f"impl {owner}: {line}" if owner else line)
        elif public and (match := re.match(r"pub\s+struct\s+(\w+)\s*(<.*?>)?", header, re.S)):
            name = match.group(1) + tidy(match.group(2) or "")
            if body is None:
                lines.append(f"struct {name}{tidy(header[match.end():])}")
            else:
                fields = [tidy(re.sub(r"^pub\s+", "", field)) for field in split_top(body, ",") if re.match(r"pub\s+(?!\()", field)]
                lines.append(f"struct {name} {{ {', '.join(fields)} }}" if fields else f"struct {name} {{ }}")
        elif public and (match := re.match(r"pub\s+enum\s+(\w+)", header)):
            variants = [re.match(r"\w+", variant).group(0) for variant in split_top(body or "", ",")]
            lines.append(f"enum {match.group(1)} {{ {', '.join(variants)} }}")
        elif public and (match := re.match(r"pub\s+(?:unsafe\s+)?trait\s+(\w+)", header)):
            methods = [signature(method) for method, _ in items(body or "")]
            lines.append(f"trait {match.group(1)} {{ {'; '.join(method for method in methods if method)} }}")
        elif public and re.match(r"pub\s+(?:const|static|type)\b", header):
            lines.append(tidy(re.sub(r"^pub\s+", "", header.split("=")[0])))
    return lines


def surface(source_dir):
    lines = []
    inside = {"crate", "self", "super"}
    for path in production_files(Path(source_dir)):
        source = without_attributes(without_comments_and_literals(path.read_text()))
        inside.update(re.findall(r"\bmod\s+(\w+)", source))
        lines += surface_of(source)
    moved = {line for line in lines if line.startswith("use ") and re.match(r"use (\w+)", line).group(1) in inside}
    return "".join(f"{line}\n" for line in sorted(set(lines) - moved))


def main():
    if len(sys.argv) != 2:
        sys.exit("usage: public_surface.py <source folder>")
    sys.stdout.write(surface(Path(sys.argv[1])))


if __name__ == "__main__":
    main()
