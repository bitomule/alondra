"""Validate production and test source boundaries on actual Rustc actions."""

load("@bazel_skylib//lib:unittest.bzl", "analysistest", "asserts")

def _rust_inputs_test_impl(ctx):
    env = analysistest.begin(ctx)
    actions = [action for action in analysistest.target_actions(env) if action.mnemonic in ["Rustc", "RustcMetadata"]]
    asserts.true(env, len(actions) > 0, "The target must register an actual Rust compile action")
    for action in actions:
        inputs = {source.short_path: True for source in action.inputs.to_list()}
        for source in ctx.attr.required_sources:
            asserts.true(env, source in inputs, "{} must compile {}".format(action.mnemonic, source))
        test_inputs = [
            source
            for source in inputs
            if source not in ctx.attr.fixture_sources and (source == "crates/test_support.rs" or (source.startswith(ctx.attr.source_dir) and (
                source.endswith("_tests.rs") or source.endswith("/tests.rs") or "/tests/" in source
            )))
        ]
        for source in ctx.attr.fixture_sources:
            asserts.true(env, source in inputs, "{} must compile the fixture {} the tests above it build on".format(action.mnemonic, source))
        for source in ctx.attr.test_only_sources:
            asserts.equals(
                env,
                ctx.attr.test_sources,
                source in inputs,
                "{} input {} must have test-only presence {}".format(action.mnemonic, source, ctx.attr.test_sources),
            )
        if not ctx.attr.test_sources:
            asserts.equals(env, [], sorted(test_inputs), "{} must not compile test files".format(action.mnemonic))
        linked = [argument.removeprefix("--extern=").split("=")[0] for argument in action.argv if argument.startswith("--extern=")]
        asserts.equals(
            env,
            [],
            [crate for crate in ctx.attr.forbidden_externs if crate in linked],
            "{} must not depend directly on these crates".format(action.mnemonic),
        )
    return analysistest.end(env)

rust_inputs_test = analysistest.make(
    _rust_inputs_test_impl,
    attrs = {
        "test_sources": attr.bool(mandatory = True),
        "source_dir": attr.string(mandatory = True),
        "required_sources": attr.string_list(mandatory = True),
        "test_only_sources": attr.string_list(default = []),
        # Test support the crate compiles for the tests of the crates above it, which have no
        # build of their own to take it from.
        "fixture_sources": attr.string_list(default = []),
        # Crates the target may not name in its own code: with one it would generate their
        # generic code itself, on every edit below it.
        "forbidden_externs": attr.string_list(default = []),
    },
)
