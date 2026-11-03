"""Aspect for collecting platform dependencies with per-target output groups.

This aspect creates individual output groups for each dependency, allowing
you to build a full app but only output specific libraries with correct
platform configuration (iOS, tvOS, watchOS).
"""

ASPECT_OUTPUT_GROUP_PREFIX = "aspect_"

_PROPAGATION_ATTRS = [
    "deps",
    "private_deps",
    "implementation_deps",
    "extension",
    "extensions",
    "frameworks",
    "app_intents",
    "watch_application",
]

def _strip_leading_chars(s, chars):
    for _ in range(len(s)):
        if s and s[0] in chars:
            s = s[1:]
        else:
            break
    return s

def _is_aspect_output_group(group_name):
    clean_name = _strip_leading_chars(group_name, "@_")
    return clean_name.startswith(ASPECT_OUTPUT_GROUP_PREFIX)

PlatformDepsInfo = provider(
    doc = "Provider for collecting platform-specific dependency outputs.",
    fields = {
        "label": "Label of the target",
        "outputs": "Depset of output files from transitive dependencies.",
    },
)

def _sanitize_label(label):
    label_str = str(label)
    label_str = _strip_leading_chars(label_str, "/@")
    sanitized = label_str.replace("/", "_").replace(":", "_").replace("-", "_").replace(".", "_").replace("+", "_")
    return ASPECT_OUTPUT_GROUP_PREFIX + sanitized

def _collect_dep_outputs(dep, transitive_outputs, transitive_output_groups):
    if PlatformDepsInfo not in dep:
        return
    transitive_outputs.append(dep[PlatformDepsInfo].outputs)
    if OutputGroupInfo in dep:
        for group_name in dir(dep[OutputGroupInfo]):
            if _is_aspect_output_group(group_name):
                output_group = getattr(dep[OutputGroupInfo], group_name, None)
                if type(output_group) == "depset":
                    existing = transitive_output_groups.get(group_name, [])
                    existing.append(output_group)
                    transitive_output_groups[group_name] = existing

def _platform_deps_aspect_impl(target, ctx):
    direct_outputs = []
    if DefaultInfo in target:
        direct_outputs = target[DefaultInfo].files.to_list()

    source_files = []
    if hasattr(ctx.rule.attr, "srcs"):
        for src in ctx.rule.attr.srcs:
            if hasattr(src, "files"):
                source_files.extend(src.files.to_list())

    transitive_outputs = []
    transitive_output_groups = {}

    for attr in _PROPAGATION_ATTRS:
        if hasattr(ctx.rule.attr, attr):
            deps = getattr(ctx.rule.attr, attr)
            if deps == None:
                continue
            deps_list = deps if type(deps) == "list" else [deps]
            for dep in deps_list:
                if dep != None:
                    _collect_dep_outputs(dep, transitive_outputs, transitive_output_groups)

    target_outputs = depset(direct_outputs + source_files, transitive = transitive_outputs)

    output_groups = {
        "platform_deps": target_outputs,
    }

    target_label = ctx.label
    target_group_name = _sanitize_label(target_label)
    output_groups[target_group_name] = target_outputs

    label_str = str(target_label)
    if ":" in label_str:
        package_path = label_str.split(":")[0]
        shorthand_name = _sanitize_label(package_path)
        if shorthand_name != target_group_name:
            output_groups[shorthand_name] = target_outputs

    for group_name, group_depsets in transitive_output_groups.items():
        output_groups[group_name] = depset(transitive = group_depsets)

    return [
        PlatformDepsInfo(
            outputs = target_outputs,
            label = target_label,
        ),
        OutputGroupInfo(**output_groups),
    ]

platform_deps_aspect = aspect(
    implementation = _platform_deps_aspect_impl,
    attr_aspects = _PROPAGATION_ATTRS,
    provides = [PlatformDepsInfo],
)
