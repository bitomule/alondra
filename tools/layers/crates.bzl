"""The only way to declare a production Rust crate: one macro per layer (docs/architecture/layers.md).

Each macro checks that its package has that layer in `crate_layers.bzl`, declares the crate with the
visibility of its layer, and adds `<name>_layer_test`, whose analysis fails when the crate, or any
crate below it, depends on a layer its own may not use. `//tools/layers:crate_layers_test` fails on a
production `rust_library` or `rust_binary` declared without them.
"""

load("//bazel/rules:rust.bzl", "rust_binary", "rust_library")
load(
    ":crate_layers.bzl",
    "ADAPTER_IN",
    "ADAPTER_OUT",
    "ADAPTER_PRIVATE",
    "ALLOWED_DEPENDENCIES",
    "APPLICATION",
    "CRATE_LAYERS",
    "DOMAIN",
    "FOUNDATION",
    "LAYERS",
    "LAYER_EXCEPTIONS",
    "PORTS",
    "PRIVATE_TO",
    "ROOT",
    "RULES",
    "ROOT_TEST_CONSUMERS",
    "layer_violation",
)

ROLE_MACROS = [
    "foundation_crate",
    "domain_crate",
    "rules_crate",
    "port_crate",
    "application_crate",
    "adapter_in_crate",
    "adapter_out_crate",
    "adapter_private_crate",
    "root_library",
    "root_binary",
]

LayerInfo = provider(
    doc = "The layer of a crate of this repository.",
    fields = {"package": "the crate's package", "layer": "its layer in crate_layers.bzl"},
)

def layer_group(layer):
    return "//tools/layers:" + layer.replace("-", "_")

def _layer_aspect_impl(target, ctx):
    package = target.label.package
    if target.label.workspace_name or package not in CRATE_LAYERS:
        return []
    if not getattr(ctx.rule.attr, "testonly", False):
        for dependency in getattr(ctx.rule.attr, "deps", []) + getattr(ctx.rule.attr, "proc_macro_deps", []):
            if LayerInfo not in dependency:
                continue
            reason = layer_violation(package, dependency[LayerInfo].package)
            if reason:
                fail("\n".join([
                    "Layer rule broken: //{}:{} ({}) depends on //{}:{} ({}).".format(
                        package,
                        target.label.name,
                        CRATE_LAYERS[package],
                        dependency.label.package,
                        dependency.label.name,
                        dependency[LayerInfo].layer,
                    ),
                    reason,
                    "Reach it through a port, or move the code; docs/architecture/layers.md explains the layers.",
                ]))
    return [LayerInfo(package = package, layer = CRATE_LAYERS[package])]

layer_aspect = aspect(
    implementation = _layer_aspect_impl,
    attr_aspects = ["deps", "proc_macro_deps"],
    doc = "Fails the analysis of a crate that depends on a layer its own may not use.",
)

def _crate_layer_test_impl(ctx):
    script = ctx.actions.declare_file(ctx.label.name + ".sh")
    ctx.actions.write(script, "#!/bin/sh\nexit 0\n", is_executable = True)
    return [DefaultInfo(executable = script)]

crate_layer_test = rule(
    implementation = _crate_layer_test_impl,
    attrs = {"crate": attr.label(mandatory = True, aspects = [layer_aspect])},
    test = True,
    doc = "Builds only when `crate` and every crate below it keep the layer rule.",
)

def _layer_table_impl(ctx):
    table = ctx.actions.declare_file(ctx.label.name + ".json")
    ctx.actions.write(table, json.encode_indent(struct(
        layers = LAYERS,
        crate_layers = CRATE_LAYERS,
        allowed_dependencies = ALLOWED_DEPENDENCIES,
        private_to = PRIVATE_TO,
        exceptions = LAYER_EXCEPTIONS,
        root_test_consumers = ROOT_TEST_CONSUMERS,
        role_macros = ROLE_MACROS,
    )))
    return [DefaultInfo(files = depset([table]))]

layer_table = rule(
    implementation = _layer_table_impl,
    doc = "crate_layers.bzl as JSON, for the test that reads the whole graph.",
)

def _visibility(package, test_visibility):
    layer = CRATE_LAYERS[package]
    if layer == ADAPTER_PRIVATE:
        users = [PRIVATE_TO[package]]
        groups = []
    else:
        users = []
        groups = [layer_group(user) for user in LAYERS if layer in ALLOWED_DEPENDENCIES[user]]
    users += sorted([user for user, dependencies in LAYER_EXCEPTIONS.items() if package in dependencies])
    return groups + ["//{}:__pkg__".format(user) for user in users] + test_visibility

def _declare(layer, rule, name, srcs, deps, crate_name, crate_root, compile_data, rustc_flags, version, visibility):
    package = native.package_name()
    declared = CRATE_LAYERS.get(package)
    if declared == None:
        fail("{} has no layer: add it to tools/layers/crate_layers.bzl before declaring {}.".format(package, name))
    if declared != layer:
        fail("{} is {} in tools/layers/crate_layers.bzl, so {} must be declared with the {} macro.".format(
            package,
            declared,
            name,
            declared,
        ))
    rule(
        name = name,
        srcs = srcs,
        deps = deps,
        crate_name = crate_name,
        crate_root = crate_root,
        compile_data = compile_data,
        rustc_flags = rustc_flags,
        edition = "2024",
        version = version,
        tags = ["crate_layer=" + layer],
        visibility = visibility,
    )
    crate_layer_test(name = name + "_layer_test", crate = ":" + name, size = "small")

def _library(layer, name, srcs, deps, crate_name, crate_root, compile_data, version, test_visibility):
    _declare(
        layer,
        rust_library,
        name,
        srcs,
        deps,
        crate_name,
        crate_root,
        compile_data,
        [],
        version,
        _visibility(native.package_name(), test_visibility),
    )

def foundation_crate(name, srcs, deps = [], crate_name = None, crate_root = None, compile_data = [], version = None, test_visibility = []):
    """A crate every layer may use; it uses only other foundation crates.

    Args:
      name: the library target.
      srcs: its production sources.
      deps: the crates it uses.
      crate_name: its Rust name, when not the target's.
      crate_root: its root file, when not `src/lib.rs`.
      compile_data: files its sources include.
      version: its package version.
      test_visibility: packages whose tests use it; the layer test still refuses their production crates.
    """
    _library(FOUNDATION, name, srcs, deps, crate_name, crate_root, compile_data, version, test_visibility)

def domain_crate(name, srcs, deps = [], crate_name = None, crate_root = None, compile_data = [], version = None, test_visibility = []):
    """A crate of the domain: it uses foundation and the domain. Arguments as `foundation_crate`."""
    _library(DOMAIN, name, srcs, deps, crate_name, crate_root, compile_data, version, test_visibility)

def rules_crate(name, srcs, deps = [], crate_name = None, crate_root = None, compile_data = [], version = None, test_visibility = []):
    """The domain's rules: what each command changes in the model's state.

    Only the roots see it, and hand it to the store through the model's `DomainRules` port.
    Arguments as `foundation_crate`.
    """
    _library(RULES, name, srcs, deps, crate_name, crate_root, compile_data, version, test_visibility)

def port_crate(name, srcs, deps = [], crate_name = None, crate_root = None, compile_data = [], version = None, test_visibility = []):
    """A port: the traits and data between the application and the adapters. Arguments as `foundation_crate`."""
    _library(PORTS, name, srcs, deps, crate_name, crate_root, compile_data, version, test_visibility)

def application_crate(name, srcs, deps = [], crate_name = None, crate_root = None, compile_data = [], version = None, test_visibility = []):
    """A piece of the application: it reaches its sisters and the adapters only through ports.

    Only the roots see it. Arguments as `foundation_crate`.
    """
    _library(APPLICATION, name, srcs, deps, crate_name, crate_root, compile_data, version, test_visibility)

def adapter_in_crate(name, srcs, deps = [], crate_name = None, crate_root = None, compile_data = [], version = None, test_visibility = []):
    """An input adapter: it calls the application's input ports. Only the roots see it. Arguments as `foundation_crate`."""
    _library(ADAPTER_IN, name, srcs, deps, crate_name, crate_root, compile_data, version, test_visibility)

def adapter_out_crate(name, srcs, deps = [], crate_name = None, crate_root = None, compile_data = [], version = None, test_visibility = []):
    """An output adapter: it implements output ports. Only the roots see it. Arguments as `foundation_crate`."""
    _library(ADAPTER_OUT, name, srcs, deps, crate_name, crate_root, compile_data, version, test_visibility)

def adapter_private_crate(name, srcs, deps = [], crate_name = None, crate_root = None, compile_data = [], version = None, test_visibility = []):
    """A part of one adapter, seen only by it (`PRIVATE_TO`) and its tests. Arguments as `foundation_crate`."""
    _library(ADAPTER_PRIVATE, name, srcs, deps, crate_name, crate_root, compile_data, version, test_visibility)

def root_library(name, srcs, deps = [], crate_name = None, crate_root = None, compile_data = [], version = None, test_visibility = []):
    """The library of a root binary, which wires the application and the adapters.

    No crate depends on a root. Arguments as `foundation_crate`.
    """
    _library(ROOT, name, srcs, deps, crate_name, crate_root, compile_data, version, test_visibility)

def root_binary(name, srcs, deps = [], crate_name = None, crate_root = None, compile_data = [], rustc_flags = [], version = None, visibility = None):
    """A binary that ships or runs from a root package.

    Args:
      name: the binary target.
      srcs: its sources.
      deps: the crates it uses.
      crate_name: its Rust name, when not the target's.
      crate_root: its root file, when not `src/main.rs`.
      compile_data: files its sources include.
      rustc_flags: extra flags, such as the linker's.
      version: its package version.
      visibility: what packages it, runs it or tests it.
    """
    _declare(ROOT, rust_binary, name, srcs, deps, crate_name, crate_root, compile_data, rustc_flags, version, visibility)
