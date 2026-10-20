#!/usr/bin/env python3
"""Cargo builds the brew release, Bazel builds everything else: a dependency a production
Bazel target links must be a normal dependency in that crate's Cargo.toml too."""
import ast
import os
import re
import sys

PRODUCTION_RULES = {"rust_binary", "rust_library", "rust_proc_macro", "rust_shared_library", "rust_static_library"}
SECTION = re.compile(r"^\[([^\]]+)\]\s*$")
ENTRY = re.compile(r"^([A-Za-z0-9_-]+)(\.workspace)?\s*=\s*(.*)$")
PATH = re.compile(r"path\s*=\s*\"([^\"]+)\"")


def normalize(name):
    return name.replace("-", "_")


def is_normal_section(section):
    return section == "dependencies" or (section.startswith("target.") and section.endswith(".dependencies"))


def cargo_sections(text):
    section = None
    for line in text.splitlines():
        line = line.split("#", 1)[0].strip()
        header = SECTION.match(line)
        if header:
            section = header.group(1).strip()
            continue
        entry = ENTRY.match(line)
        if entry and section:
            yield section, entry.group(1), entry.group(2) is not None, entry.group(3)


def workspace(root):
    with open(os.path.join(root, "Cargo.toml")) as manifest:
        text = manifest.read()
    members = re.search(r"members\s*=\s*\[([^\]]*)\]", text).group(1)
    paths = {
        normalize(name): os.path.normpath(PATH.search(value).group(1))
        for section, name, _, value in cargo_sections(text)
        if section == "workspace.dependencies" and PATH.search(value)
    }
    return re.findall(r"\"([^\"]+)\"", members), paths


def normal_dependencies(root, member, workspace_paths):
    with open(os.path.join(root, member, "Cargo.toml")) as manifest:
        text = manifest.read()
    names, local = set(), set()
    for section, name, inherited, value in cargo_sections(text):
        if not is_normal_section(section):
            continue
        package = re.search(r"package\s*=\s*\"([^\"]+)\"", value)
        names.add(normalize(package.group(1) if package else name))
        path = PATH.search(value)
        if path:
            local.add(os.path.normpath(os.path.join(member, path.group(1))))
        elif inherited or "workspace = true" in value:
            if normalize(name) in workspace_paths:
                local.add(workspace_paths[normalize(name)])
    return names, local


def labels(node):
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        yield node.value
    elif isinstance(node, (ast.List, ast.Tuple)):
        for element in node.elts:
            yield from labels(element)
    elif isinstance(node, ast.BinOp):
        yield from labels(node.left)
        yield from labels(node.right)


def uses_dev_dependencies(node):
    return any(
        isinstance(call, ast.Call) and any(keyword.arg in ("dev", "normal_dev") for keyword in call.keywords)
        for call in ast.walk(node)
    )


def production_targets(root, member):
    with open(os.path.join(root, member, "BUILD.bazel")) as build:
        tree = ast.parse(build.read())
    for call in ast.walk(tree):
        if isinstance(call, ast.Call) and isinstance(call.func, ast.Name) and call.func.id in PRODUCTION_RULES:
            keywords = {keyword.arg: keyword.value for keyword in call.keywords}
            name = keywords["name"].value
            if ("testonly" in keywords and getattr(keywords["testonly"], "value", True)) or ("deps" in keywords and uses_dev_dependencies(keywords["deps"])):
                continue
            deps = list(labels(keywords["deps"])) if "deps" in keywords else []
            yield name, deps


def mismatches(root):
    members, workspace_paths = workspace(root)
    problems = []
    for member in members:
        names, local = normal_dependencies(root, member, workspace_paths)
        for target, deps in production_targets(root, member):
            for label in deps:
                if label.startswith("//crates/"):
                    package = label[2:].split(":", 1)[0]
                    if package != member and package not in local:
                        problems.append(f"//{member}:{target} links {label}, but {member}/Cargo.toml lacks {package} in [dependencies]")
                elif label.startswith("@crates//:"):
                    crate = normalize(label.split(":", 1)[1])
                    if crate not in names:
                        problems.append(f"//{member}:{target} links {label}, but {member}/Cargo.toml lacks {crate} in [dependencies]")
    return problems


def main():
    root = os.path.join(os.environ["TEST_SRCDIR"], os.environ["TEST_WORKSPACE"]) if "TEST_SRCDIR" in os.environ else os.getcwd()
    members, _ = workspace(root)
    missing = [member for member in members if not os.path.exists(os.path.join(root, member, "BUILD.bazel"))]
    if missing:
        print("Add these workspace members to //crates:cargo_bazel_deps_test data: " + ", ".join(missing))
        return 1
    problems = mismatches(root)
    for problem in problems:
        print(problem)
    print(f"{len(members)} crates checked, {len(problems)} mismatches")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
