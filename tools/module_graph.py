#!/usr/bin/env python3
"""Maps the modules of a crate: lines, tests, the modules each one uses, and the groups of modules
that depend on each other in a cycle. A cycle has to be cut before its modules can live in separate
crates. Reads sources only; nothing is built.

    python3 tools/module_graph.py crates/daemon-engine/src"""
import collections
import re
import sys
from pathlib import Path


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


def main():
    found = modules(Path(sys.argv[1]))
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
    main()
