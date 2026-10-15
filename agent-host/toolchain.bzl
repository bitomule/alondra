"""Expose the hermetic runtime selected by the registered Node toolchain."""

def _pinned_node_impl(ctx):
    info = ctx.toolchains["@rules_nodejs//nodejs:runtime_toolchain_type"].nodeinfo
    if not info.node or info.node_path:
        fail("The host laboratory requires the pinned hermetic Node artifact")
    return [DefaultInfo(files = depset([info.node]), runfiles = ctx.runfiles(files = [info.node]))]

pinned_node = rule(
    implementation = _pinned_node_impl,
    toolchains = ["@rules_nodejs//nodejs:runtime_toolchain_type"],
)
