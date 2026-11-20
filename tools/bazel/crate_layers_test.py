#!/usr/bin/env python3
"""The layer rule over the whole Rust graph (docs/architecture/layers.md).

Fails when a production rust_library or rust_binary is declared without a role macro, when a package
that ships Rust has no layer or a layer names a package with no crate, when a production dependency
breaks the rule (in any branch of a select), when a test outside the root depends on a crate or
binary of the root, when an exception list keeps an entry nothing uses or grows past its ceiling, and
when a test-only library builds a production crate a second time.
"""
import json
import os
import sys
import xml.etree.ElementTree as ElementTree

# The number of entries in LAYER_EXCEPTIONS. It only goes down: lower it with every entry removed.
EXCEPTION_CEILING = 56

# The number of packages in ROOT_TEST_CONSUMERS, which only goes down too.
ROOT_TEST_CONSUMER_CEILING = 5

# rules_rust compiles a crate's build.rs into a binary of its own.
BUILD_SCRIPT_MACROS = {"cargo_build_script"}


def package_of(label):
    return label.removeprefix("//").split(":")[0]


def read_targets(path):
    targets = {}
    for rule in ElementTree.parse(path).getroot().iter("rule"):
        attributes = {child.get("name"): child for child in rule}
        testonly = attributes.get("testonly")
        crate_name = attributes.get("crate_name")
        generator = attributes.get("generator_function")
        deps = []
        for name in ("deps", "proc_macro_deps"):
            if name in attributes:
                deps += [label.get("value") for label in attributes[name].iter("label")]
        targets[rule.get("name")] = {
            "kind": rule.get("class"),
            "location": rule.get("location"),
            "testonly": testonly is not None and testonly.get("value") == "true",
            "crate_name": crate_name.get("value") if crate_name is not None else rule.get("name").split(":")[-1],
            "generator": generator.get("value") if generator is not None else None,
            "deps": deps,
        }
    return targets


def violation(table, package, dependency):
    if package == dependency or dependency in table["exceptions"].get(package, []):
        return None
    layer = table["crate_layers"][package]
    dependency_layer = table["crate_layers"][dependency]
    if dependency_layer == "adapter-private":
        owner = table["private_to"][dependency]
        return None if owner == package else f"{dependency} is private to the adapter {owner}"
    if dependency_layer in table["allowed_dependencies"][layer]:
        return None
    return f"a crate of layer {layer} may depend only on layers {', '.join(table['allowed_dependencies'][layer])}"


def second_builds(targets):
    """A test-only library that builds a production crate again, as the `test-hooks` builds did:
    every crate between it and the tests compiles twice, and an edit reruns both. A test registers
    the doubles it needs at run time instead."""
    production = {
        target["crate_name"]: label
        for label, target in targets.items()
        if not target["testonly"] and target["kind"] == "rust_library"
    }
    return [
        f"{label} builds the crate of {production[target['crate_name']]} a second time for tests: "
        "register the test's doubles at run time instead (docs/architecture/build-graph.md)."
        for label, target in sorted(targets.items())
        if target["testonly"] and target["kind"] == "rust_library" and target["crate_name"] in production
    ]


def read_rules(path):
    """Every rule with its kind and the labels it reads, implicit ones included."""
    rules = {}
    for rule in ElementTree.parse(path).getroot().iter("rule"):
        rules[rule.get("name")] = (rule.get("class"), [entry.get("name") for entry in rule.iter("rule-input")])
    return rules


def root_reached(table, rules):
    """For each test that reaches a crate or a binary of the root, through anything it reads at any
    depth, the first one it reaches."""
    roots = {package for package, layer in table["crate_layers"].items() if layer == "root"}
    reached = {}

    def reach(label):
        if label not in reached:
            kind, inputs = rules.get(label, ("", []))
            reached[label] = None
            if kind in ("rust_library", "rust_binary") and package_of(label) in roots:
                reached[label] = label
            else:
                reached[label] = next(filter(None, map(reach, inputs)), None)
        return reached[label]

    limit = sys.getrecursionlimit()
    sys.setrecursionlimit(max(limit, len(rules) + 1000))
    try:
        return {label: reach(label) for label, (kind, _) in sorted(rules.items()) if kind.endswith("_test") and reach(label)}
    finally:
        sys.setrecursionlimit(limit)


def tests_above_their_layer(table, rules):
    """A test outside the root that depends on a crate or a binary of the root, through `deps` or
    `data` at any depth: it would rerun on every edit of every crate the daemon links."""
    found = []
    roots = sorted(package for package, layer in table["crate_layers"].items() if layer == "root")
    consumers = table["root_test_consumers"]
    used = set()
    for label, crate in root_reached(table, rules).items():
        package = package_of(label)
        if package in roots:
            continue
        if package in consumers:
            used.add(package)
            continue
        found.append(
            f"{label} depends on {crate}: a test outside the root ({', '.join(roots)}) may not depend on a "
            "crate or binary of the root, or it reruns on every edit of the daemon. Test against the port or "
            "its double, or move the test to the root (docs/architecture/layers.md)."
        )
    for package in sorted(set(consumers) - used):
        found.append(f"No test of {package} depends on the root any more: remove it from ROOT_TEST_CONSUMERS.")
    if len(consumers) > ROOT_TEST_CONSUMER_CEILING:
        found.append(
            f"ROOT_TEST_CONSUMERS has {len(consumers)} packages, over its ceiling of {ROOT_TEST_CONSUMER_CEILING}: "
            "the list only shrinks. Test against the port or its double, or move the test to the root."
        )
    elif len(consumers) < ROOT_TEST_CONSUMER_CEILING:
        found.append(f"ROOT_TEST_CONSUMERS has {len(consumers)} packages: lower ROOT_TEST_CONSUMER_CEILING to {len(consumers)}.")
    return found


def problems(table, targets):
    found = []
    macros = set(table["role_macros"])
    production = {label: target for label, target in targets.items() if not target["testonly"]}
    for label, target in sorted(production.items()):
        if target["generator"] not in macros | BUILD_SCRIPT_MACROS:
            found.append(
                f"{label} ({target['location']}) is a production {target['kind']} declared "
                f"{'by ' + target['generator'] if target['generator'] else 'directly'}: declare it with "
                f"the macro of its layer ({', '.join(table['role_macros'])}) from tools/bazel/crates.bzl, "
                "or mark it testonly."
            )
    shipping = {package_of(label) for label in production}
    for package in sorted(shipping - table["crate_layers"].keys()):
        found.append(f"{package} ships Rust but has no layer in tools/bazel/crate_layers.bzl.")
    for package in sorted(table["crate_layers"].keys() - shipping):
        found.append(f"{package} has a layer in tools/bazel/crate_layers.bzl but no production crate: remove it.")
    used = set()
    for label, target in sorted(production.items()):
        package = package_of(label)
        if package not in table["crate_layers"]:
            continue
        for dependency in target["deps"]:
            dependency_package = package_of(dependency)
            if not dependency.startswith("//") or dependency_package not in table["crate_layers"]:
                continue
            if dependency_package in table["exceptions"].get(package, []):
                used.add((package, dependency_package))
            reason = violation(table, package, dependency_package)
            if reason:
                found.append(f"{label} depends on {dependency}: {reason}.")
    found += second_builds(targets)
    entries = {(package, dependency) for package, dependencies in table["exceptions"].items() for dependency in dependencies}
    for package, dependency in sorted(entries - used):
        found.append(
            f"The exception {package} -> {dependency} is no longer used: remove it from LAYER_EXCEPTIONS "
            "and lower EXCEPTION_CEILING."
        )
    if len(entries) > EXCEPTION_CEILING:
        found.append(
            f"LAYER_EXCEPTIONS has {len(entries)} entries, over its ceiling of {EXCEPTION_CEILING}: "
            "the list only shrinks. Reach the crate through a port instead."
        )
    elif len(entries) < EXCEPTION_CEILING:
        found.append(f"LAYER_EXCEPTIONS has {len(entries)} entries: lower EXCEPTION_CEILING to {len(entries)}.")
    return found


def main():
    with open(os.environ["CRATE_LAYERS"]) as file:
        table = json.load(file)
    found = problems(table, read_targets(os.environ["RUST_TARGETS"]))
    found += tests_above_their_layer(table, read_rules(os.environ["RULES"]))
    for problem in found:
        print(problem)
    return 1 if found else 0


if __name__ == "__main__":
    sys.exit(main())
