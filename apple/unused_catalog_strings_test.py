#!/usr/bin/env python3
"""Every entry in the apps' string catalogs is a string literal somewhere in the Swift sources.

A catalog key matches a literal once both spell their arguments the same way: a format specifier
(`%@`, `%lld`, `%1$@`) in the key and an interpolation (`\\(x)`) in the literal both become one
placeholder. An entry the code reaches some other way goes in KEPT with the reason."""
import json
import re
import sys

KEPT = {}

FORMAT = re.compile(r"%(?:\d+\$)?[-+ #0']*\d*(?:\.\d+)?(?:hh|h|ll|l|q|z|t|j)?[@dDuUxXoOfeEgGcCsSpaAi]")
PLACEHOLDER = "\x00"
ESCAPES = {"n": "\n", "t": "\t", "r": "\r", "0": "\0", '"': '"', "'": "'", "\\": "\\"}


def normalized_key(key):
    return FORMAT.sub(PLACEHOLDER, key.replace("%%", "%"))


class Lexer:
    """Collects every string literal in Swift source, interpolations included, outside comments."""

    def __init__(self, text):
        self.text = text
        self.at = 0
        self.literals = []

    def code(self, closing=None):
        depth = 0
        text = self.text
        while self.at < len(text):
            char = text[self.at]
            if text.startswith("//", self.at):
                end = text.find("\n", self.at)
                self.at = len(text) if end < 0 else end
            elif text.startswith("/*", self.at):
                self.block_comment()
            elif char == '"' or (char == "#" and re.match(r'#+"', text[self.at:])):
                self.literal()
            elif closing and char == "(":
                depth += 1
                self.at += 1
            elif closing and char == ")":
                self.at += 1
                if depth == 0:
                    return
                depth -= 1
            else:
                self.at += 1

    def block_comment(self):
        depth = 0
        while self.at < len(self.text):
            if self.text.startswith("/*", self.at):
                depth += 1
                self.at += 2
            elif self.text.startswith("*/", self.at):
                depth -= 1
                self.at += 2
                if depth == 0:
                    return
            else:
                self.at += 1

    def literal(self):
        hashes = len(re.match(r"#*", self.text[self.at:]).group())
        self.at += hashes
        multiline = self.text.startswith('"""', self.at)
        self.at += 3 if multiline else 1
        delimiter = ('"""' if multiline else '"') + "#" * hashes
        escape = "\\" + "#" * hashes
        parts = []
        text = self.text
        while self.at < len(text):
            if text.startswith(delimiter, self.at):
                self.at += len(delimiter)
                break
            if text.startswith(escape, self.at):
                self.at += len(escape)
                char = text[self.at]
                if char == "(":
                    self.at += 1
                    self.code(closing=")")
                    parts.append(PLACEHOLDER)
                elif char == "u" and text.startswith("{", self.at + 1):
                    end = text.index("}", self.at)
                    parts.append(chr(int(text[self.at + 2:end], 16)))
                    self.at = end + 1
                elif multiline and char == "\n":
                    self.at += 1
                    parts.append("\\\n")
                else:
                    parts.append(ESCAPES.get(char, char))
                    self.at += 1
            else:
                parts.append(text[self.at])
                self.at += 1
        value = "".join(parts)
        self.literals.append(dedented(value) if multiline else value)


def dedented(value):
    lines = value.split("\n")[1:]
    indent = lines.pop() if lines else ""
    return "\n".join(line[len(indent):] for line in lines).replace("\\\n", "")


def literals(paths):
    found = set()
    for path in paths:
        lexer = Lexer(open(path, encoding="utf-8").read())
        lexer.code()
        found.update(lexer.literals)
    return found


def main():
    catalogs = sys.argv[1:5]
    used = {normalized_key(literal) for literal in literals(sys.argv[5:])}
    unused = []
    for path in catalogs:
        for key in json.load(open(path, encoding="utf-8"))["strings"]:
            if key not in KEPT and normalized_key(key) not in used:
                unused.append(f"{path}: {key!r} is not used by any Swift source")
    if unused:
        print("\n".join(unused), file=sys.stderr)
        print(f"{len(unused)} catalog entries no code uses: delete them, or add them to KEPT with the reason.", file=sys.stderr)
        sys.exit(1)


main()
