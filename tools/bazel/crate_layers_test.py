#!/usr/bin/env python3
"""The layer rule over the whole Rust graph (docs/architecture/layers.md).

Fails when a production rust_library or rust_binary is declared without a role macro, when a package
that ships Rust has no layer or a layer names a package with no crate, when a production dependency
breaks the rule (in any branch of a select), and when the exception list keeps an entry nothing uses
or grows past its ceiling.
"""
import json
import os
import sys
import xml.etree.ElementTree as ElementTree

# The number of entries in LAYER_EXCEPTIONS. It only goes down: lower it with every entry removed.
EXCEPTION_CEILING = 55
# The number of entries in TEST_HOOK_EXCEPTIONS, which only goes down too.
TEST_HOOK_EXCEPTION_CEILING = 2

# rules_rust compiles a crate's build.rs into a binary of its own.
BUILD_SCRIPT_MACROS = {"cargo_build_script"}


def package_of(label):
    return label.removeprefix("//").split(":")[0]


def read_targets(path):
    targets = {}
    for rule in ElementTree.parse(path).getroot().iter("rule"):
        attributes = {child.get("name"): child for child in rule}
        testonly = attributes.get("testonly")
        generator = attributes.get("generator_function")
        deps = []
        for name in ("deps", "proc_macro_deps"):
            if name in attributes:
                deps += [label.get("value") for label in attributes[name].iter("label")]
        targets[rule.get("name")] = {
            "kind": rule.get("class"),
            "location": rule.get("location"),
            "testonly": testonly is not None and testonly.get("value") == "true",
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


def hooks_linking_adapters(table, targets):
    """A testonly library linking an output adapter its production crate may not use: every test
    above those hooks would rerun on each edit of the adapter."""
    found = []
    used = set()
    allowed = table["test_hook_exceptions"]
    for label, target in sorted(targets.items()):
        package = package_of(label)
        if not target["testonly"] or target["kind"] != "rust_library" or package not in table["crate_layers"]:
            continue
        for dependency in target["deps"]:
            dependency_package = package_of(dependency)
            if table["crate_layers"].get(dependency_package) != "adapter-out":
                continue
            if dependency_package in allowed.get(package, []):
                used.add((package, dependency_package))
                continue
            reason = violation(table, package, dependency_package)
            if reason:
                found.append(
                    f"{label} depends on {dependency}: test hooks may not link an output adapter their crate "
                    f"may not use ({reason}). Keep the fakes that need it under #[cfg(test)], or fake the port."
                )
    entries = {(package, dependency) for package, dependencies in allowed.items() for dependency in dependencies}
    for package, dependency in sorted(entries - used):
        found.append(
            f"The test hook exception {package} -> {dependency} is no longer used: remove it from TEST_HOOK_EXCEPTIONS."
        )
    if len(entries) > TEST_HOOK_EXCEPTION_CEILING:
        found.append(
            f"TEST_HOOK_EXCEPTIONS has {len(entries)} entries, over its ceiling of {TEST_HOOK_EXCEPTION_CEILING}: "
            "the list only shrinks. Keep the fakes that need the adapter under #[cfg(test)], or fake the port."
        )
    elif len(entries) < TEST_HOOK_EXCEPTION_CEILING:
        found.append(f"TEST_HOOK_EXCEPTIONS has {len(entries)} entries: lower TEST_HOOK_EXCEPTION_CEILING to {len(entries)}.")
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
    found += hooks_linking_adapters(table, targets)
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
    for problem in found:
        print(problem)
    return 1 if found else 0


if __name__ == "__main__":
    sys.exit(main())
