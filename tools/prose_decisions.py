#!/usr/bin/env python3
"""Finds control decisions that read prose in production Rust, TypeScript and Swift.

The rule: a branch that changes behaviour decides on a type, never on prose. The inventory of
known cases is the allowlist next to this file.
Matching is syntactic, line by line, with comments and test code left out. Every match is either
in the allowlist (with its category and reason) or a failure; an allowlist entry that matches
nothing is a failure too, so the list only shrinks.
"""
import os
import re
import sys
from dataclasses import dataclass
from pathlib import Path

ALLOWLIST = "tools/prose_decisions_allowlist.tsv"
CORPUS = "tools/testdata/prose_decisions_corpus.tsv"

CATEGORIES = {"display", "log", "user-search", "format", "own-grammar", "security-parse"}
FINDING = re.compile(r"^finding:[A-Z]-\d{2}$|^finding:codex-mcp-denial$")

SCANNED = {
    "rust": ["crates"],
    "ts": ["agent-host", "plugin/hooks", "web/app"],
    "swift": ["apple"],
}
EXTENSIONS = {"rust": (".rs",), "ts": (".ts", ".tsx", ".mts", ".mjs"), "swift": (".swift",)}
SKIPPED_DIRS = {
    "node_modules", "target", "build", ".build", "DerivedData", "dist",
    "tests", "benches", "Tests", "Testing", "NativeHarness", "testdata", "fixtures",
    "ProcessTests", "ProcessFixtures",
}
SKIPPED_NAME = re.compile(
    r"(^tests\.rs$|_tests?\.rs$|^test_support|^fake|\.test\.(ts|mts|mjs)$|fixture|Tests?\.swift$)"
)

# A string literal that holds two or more words.
PHRASE = r'"[^"\\]*\w\s[^"\\]*"'
PHRASE_TS = r"""(?:"[^"\\]*\w[ ]+\w[^"\\]*"|'[^'\\]*\w[ ]+\w[^'\\]*'|`[^`\\$]*\w[ ]+\w[^`\\$]*`)"""
PROSE_SOURCE = r"(?:to_string\(\)|stderr|stdout|message|reason|detail|error|errors|text|why|need|body|label|prompt)"
CHAINED = r"(?:\??\.\w+(?:\([^()]*\))?|\[[^\]]*\])*?"
QUALIFIED = r"(?:\w+::)*"
# A literal of punctuation and a space, such as a list bullet or the end of a sentence.
PUNCTUATED = r'"[^\w"\\]{1,2} "'

PATTERNS = {
    "rust": [
        ("phrase-method", re.compile(
            r"\.(?:contains|starts_with|ends_with|find|rfind|strip_prefix|strip_suffix|split_once|rsplit_once|matches)"
            r"\(\s*&?(?:" + PHRASE + "|" + PUNCTUATED + ")")),
        ("phrase-equality", re.compile(r"[!=]=\s*&?" + PHRASE + r"|" + PHRASE + r"\s*[!=]=")),
        ("lowercase", re.compile(r"\.to_(?:ascii_)?lowercase\(\)")),
        ("prose-method", re.compile(
            r"\b" + PROSE_SOURCE + CHAINED
            + r"(?:\.(?:contains|starts_with|ends_with|strip_prefix|strip_suffix)\((?!\s*'|\s*char::)|\.find\(\s*\")")),
        ("match-to-string", re.compile(r"\bmatch\b.*to_string\(\)\.as_str\(\)")),
        ("format-equality", re.compile(r"[!=]=\s*&?(?:Some\()?format!\(|format!\([^;]*\)\s*[!=]=")),
        ("regex", re.compile(r"\bRegex(?:Builder|Set)?::new\b")),
        ("phrase-list", re.compile(
            r"(?:\b\w+\(\s*\w+\s*,\s*&|\bin\s+)\[\s*" + PHRASE + r"\s*,\s*" + PHRASE)),
        ("const-prefix", re.compile(
            r"\.(?:starts_with|strip_prefix)\(\s*&?" + QUALIFIED + r"[A-Z][A-Z0-9_]{3,}\s*\)")),
        ("error-list", re.compile(r"\b[A-Z][A-Z0-9_]*(?:ERRORS|PROMPTS)\.(?:contains|iter)\(")),
        ("markup-match", re.compile(r"\.(?:contains|starts_with|ends_with)\(\s*\"[^\"]*</")),
        ("process-text", re.compile(r"Command::new\(\"(?:/bin/)?ps\"\)")),
    ],
    "ts": [
        ("error-message", re.compile(r"\.message\b(?!\s*[:=][^=])|\bString\(\s*(?:err|error|e)\s*\)")),
        ("stdio-equality", re.compile(r"\b(?:stderr|stdout)\b.*(?:===|!==)|(?:===|!==).*\b(?:stderr|stdout)\b")),
        ("phrase-method", re.compile(
            r"\.(?:includes|startsWith|endsWith|indexOf|match|search)\(\s*" + PHRASE_TS)),
        ("regex-test", re.compile(r"/[^/\n]*\w[ ]+\w[^/\n]*/[gimsuy]*\.(?:test|exec)\(")),
        ("prose-method", re.compile(
            r"\b(?:stderr|stdout|message|reason|detail|text)\b" + r"(?:\??\.\w+(?:\([^()]*\))?)*?"
            + r"\.(?:includes|startsWith|endsWith|match|search)\(")),
    ],
    "swift": [
        ("localized-description", re.compile(r"\blocalizedDescription\b")),
        ("describing-error", re.compile(r"String\(describing:\s*[^)]*(?:error|Error|userInfo)")),
        ("case-insensitive", re.compile(r"\blocalizedCaseInsensitiveContains\(|\.lowercased\(\)\.contains\(")),
        ("phrase-method", re.compile(r"\.(?:contains|hasPrefix|hasSuffix|range\(of:)\(?\s*" + PHRASE)),
        ("phrase-equality", re.compile(r"[!=]=\s*" + PHRASE + r"|" + PHRASE + r"\s*[!=]=")),
        ("text-field", re.compile(
            r"\b(?:detail|message|reason)\??\s*(?:[!=]=(?!\s*(?:\.|nil\b))|\.(?:isEmpty|hasPrefix|hasSuffix|contains)\b)"
            r"|\.label\s*[!=]=\s*\w+\.label\b|\.value\.(?:hasPrefix|hasSuffix)\(")),
    ],
}


@dataclass(frozen=True)
class Match:
    path: str
    line: int
    text: str
    pattern: str

    @property
    def fingerprint(self):
        return normalized(self.text)


def normalized(text):
    return " ".join(text.split())


# alondrad's tests also build the support crate with its `test-hooks` feature.
TEST_ONLY = ("#[cfg(test)]", '#[cfg(any(test, feature = "test-hooks"))]')


def without_cfg_test(text):
    """Blanks every item only tests compile, keeping line breaks so line numbers still count."""
    out = list(text)
    i = 0
    while True:
        found = [(at, len(marker)) for marker in TEST_ONLY if (at := text.find(marker, i)) >= 0]
        if not found:
            return "".join(out)
        i, length = min(found)
        j = i + length
        depth, in_string, escaped = 0, False, False
        while j < len(text):
            c = text[j]
            if in_string:
                if c == "\\" and not escaped:
                    escaped = True
                    j += 1
                    continue
                if c == '"' and not escaped:
                    in_string = False
                escaped = False
            elif c == "'" and j + 2 < len(text) and text[j + 2] == "'":
                j += 2
            elif c == "'" and j + 3 < len(text) and text[j + 1] == "\\" and text[j + 3] == "'":
                j += 3
            elif c == '"':
                in_string = True
            elif c == "{":
                depth += 1
            elif c == "}":
                depth -= 1
                if depth == 0:
                    break
            elif c == ";" and depth == 0:
                break
            j += 1
        for k in range(i, min(j + 1, len(text))):
            if out[k] != "\n":
                out[k] = " "
        i = j + 1


def without_comments(text):
    """Blanks `//` and `/* */` comments outside string literals, keeping line breaks."""
    out = []
    i, n = 0, len(text)
    quote = None
    while i < n:
        c = text[i]
        if quote:
            out.append(c)
            if c == "\\" and i + 1 < n:
                out.append(text[i + 1])
                i += 2
                continue
            if c == quote:
                quote = None
            i += 1
            continue
        if c in "\"`":
            quote = c
        elif c == "/" and text.startswith("//", i):
            end = text.find("\n", i)
            end = n if end < 0 else end
            out.append(" " * (end - i))
            i = end
            continue
        elif c == "/" and text.startswith("/*", i):
            end = text.find("*/", i + 2)
            end = n if end < 0 else end + 2
            out.append("".join(ch if ch == "\n" else " " for ch in text[i:end]))
            i = end
            continue
        out.append(c)
        i += 1
    return "".join(out)


def production_text(language, text):
    if language == "rust":
        text = without_cfg_test(text)
    return without_comments(text)


def matches_in(language, path, text):
    found = []
    for number, line in enumerate(production_text(language, text).splitlines(), 1):
        for name, pattern in PATTERNS[language]:
            if pattern.search(line):
                found.append(Match(path, number, line.strip(), name))
                break
    return found


def language_of(path):
    for language, suffixes in EXTENSIONS.items():
        if path.name.endswith(suffixes):
            return language
    return None


def walk(base):
    for directory, folders, files in os.walk(base):
        folders[:] = sorted(f for f in folders if f not in SKIPPED_DIRS and not f.startswith("."))
        for name in sorted(files):
            yield Path(directory) / name


def sources(root):
    for language, folders in SCANNED.items():
        for folder in folders:
            base = root / folder
            if not base.exists():
                continue
            for path in walk(base):
                relative = path.relative_to(root)
                if language_of(path) != language:
                    continue
                if SKIPPED_NAME.search(path.name) or path.name.endswith(".d.ts"):
                    continue
                yield language, relative.as_posix(), path


def scan(root):
    found = []
    for language, relative, path in sources(root):
        found.extend(matches_in(language, relative, path.read_text(encoding="utf-8", errors="replace")))
    return found


@dataclass(frozen=True)
class Entry:
    path: str
    fingerprint: str
    category: str
    reason: str
    line: int


def read_allowlist(path):
    entries, problems = [], []
    for number, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if not raw.strip() or raw.startswith("#"):
            continue
        fields = raw.split("\t")
        if len(fields) != 4 or not all(f.strip() for f in fields):
            problems.append(f"{path.name}:{number}: needs path, line, category and reason, tab-separated")
            continue
        file, fingerprint, category, reason = fields
        if category not in CATEGORIES and not FINDING.match(category):
            problems.append(f"{path.name}:{number}: unknown category {category!r}")
            continue
        entries.append(Entry(file, normalized(fingerprint), category, reason, number))
    return entries, problems


def compare(found, entries):
    """Matches the allowlist does not name, and allowlist entries that no longer match."""
    allowed = {(e.path, e.fingerprint) for e in entries}
    seen = {(m.path, m.fingerprint) for m in found}
    new = [m for m in found if (m.path, m.fingerprint) not in allowed]
    stale = [e for e in entries if (e.path, e.fingerprint) not in seen]
    keys = [(e.path, e.fingerprint) for e in entries]
    duplicates = sorted({k for k in keys if keys.count(k) > 1})
    return new, stale, duplicates


def read_corpus(path):
    cases = []
    for number, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if not raw.strip() or raw.startswith("#"):
            continue
        finding, language, snippet = raw.split("\t", 2)
        cases.append((number, finding, language, snippet))
    return cases


def undetected(cases):
    return [
        (number, finding, snippet)
        for number, finding, language, snippet in cases
        if not matches_in(language, "corpus", snippet)
    ]


def report(root):
    problems = []
    entries, malformed = read_allowlist(root / ALLOWLIST)
    problems.extend(malformed)
    new, stale, duplicates = compare(scan(root), entries)
    for m in new:
        problems.append(
            f"{m.path}:{m.line}: [{m.pattern}] {m.text}\n"
            "    A control decision must not read prose (AGENTS.md, Code). Decide on a typed signal;"
            " if the line only displays, logs or parses a documented format, add it to"
            f" {ALLOWLIST} with its category and reason."
        )
    for e in stale:
        problems.append(f"{ALLOWLIST}:{e.line}: matches nothing in {e.path}; remove the entry: {e.fingerprint}")
    for path, fingerprint in duplicates:
        problems.append(f"{ALLOWLIST}: listed twice: {path}\t{fingerprint}")
    for number, finding, snippet in undetected(read_corpus(root / CORPUS)):
        problems.append(f"{CORPUS}:{number}: {finding} is no longer detected: {snippet}")
    return problems


def main(argv):
    root = Path(argv[1]) if len(argv) > 1 else Path(__file__).resolve().parents[1]
    if len(argv) > 2 and argv[2] == "--seed":
        entries, _ = read_allowlist(root / ALLOWLIST)
        new, _, _ = compare(scan(root), entries)
        for m in new:
            print(f"{m.path}\t{m.fingerprint}\t?\t{m.pattern}")
        return 0
    problems = report(root)
    for problem in problems:
        print(problem, file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
