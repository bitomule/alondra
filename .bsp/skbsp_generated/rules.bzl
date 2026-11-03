"""Wrapper rule that applies platform_deps_aspect via rule attribute.

By applying the aspect through a rule attribute (not CLI --aspects),
the ConfiguredTargetKey is stable across builds, preventing potential
Skyframe cache invalidation (bazelbuild/bazel#19914).

No configuration transition is applied — the app target uses its own
internal transitions (from ios_application/tvos_application/etc.) to
configure deps with correct platform settings. This ensures the wrapper
reuses the same ConfiguredTargetValue nodes as a normal app build.
"""

load(":aspect.bzl", "platform_deps_aspect")

def _platform_deps_wrapper_impl(ctx):
    target = ctx.attr.target

    # Forward OutputGroupInfo so --output_groups works on wrapper targets
    output_groups = {}
    if OutputGroupInfo in target:
        for group_name in dir(target[OutputGroupInfo]):
            group = getattr(target[OutputGroupInfo], group_name, None)
            if type(group) == "depset":
                output_groups[group_name] = group

    return [
        DefaultInfo(files = target[DefaultInfo].files),
        OutputGroupInfo(**output_groups),
    ]

_platform_deps_wrapper = rule(
    implementation = _platform_deps_wrapper_impl,
    attrs = {
        "target": attr.label(
            aspects = [platform_deps_aspect],
            mandatory = True,
        ),
    },
    doc = "Wrapper that applies platform_deps_aspect via rule attribute for stable caching.",
)

def platform_deps_wrapper(name, target, testonly = False, visibility = None):
    """Create a wrapper target for an app.

    Args:
        name: Base name for the wrapper target.
        target: The app target label.
        testonly: If True, marks the wrapper as testonly (required for test targets).
        visibility: Visibility for the generated target.
    """
    _platform_deps_wrapper(
        name = name,
        target = target,
        testonly = testonly,
        tags = ["manual"],
        visibility = visibility or ["//visibility:public"],
    )
