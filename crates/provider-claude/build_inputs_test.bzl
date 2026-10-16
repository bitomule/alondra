"""Validate provider source boundaries on registered Rust compile actions."""

load("@bazel_skylib//lib:unittest.bzl", "analysistest", "asserts")

def _rust_inputs_test_impl(ctx):
    env = analysistest.begin(ctx)
    actions = [action for action in analysistest.target_actions(env) if action.mnemonic in ["Rustc", "RustcMetadata"]]
    asserts.true(env, len(actions) > 0, "The target must register an actual Rust compile action")
    required = [
        "crates/provider-claude/src/lib.rs",
        "crates/provider-claude/src/adapter.rs",
        "crates/provider-claude/src/normalizer.rs",
        "crates/provider-claude/src/records.rs",
        "crates/provider-claude/src/wire.rs",
    ]
    component = [
        "crates/provider-claude/src/controller_tests.rs",
        "crates/provider-claude/src/facade_tests.rs",
        "crates/test_support.rs",
        "crates/agent-application/tests/support/controller.rs",
        "crates/alondrad/src/domain_store/schema.sql",
        "crates/agent-protocol/fixtures/hello.json",
    ]
    for action in actions:
        inputs = {source.short_path: True for source in action.inputs.to_list()}
        for source in required:
            asserts.true(env, source in inputs, "{} must compile {}".format(action.mnemonic, source))
        for source in component:
            asserts.equals(
                env,
                ctx.attr.test_kind != "production",
                source in inputs,
                "{} input {} must have test-only presence {}".format(action.mnemonic, source, ctx.attr.test_kind != "production"),
            )
        source = "crates/provider-claude/src/sdk_tests.rs"
        asserts.equals(
            env,
            ctx.attr.test_kind == "sdk",
            source in inputs,
            "{} input {} must have SDK-only presence {}".format(action.mnemonic, source, ctx.attr.test_kind == "sdk"),
        )
    return analysistest.end(env)

rust_inputs_test = analysistest.make(
    _rust_inputs_test_impl,
    attrs = {"test_kind": attr.string(mandatory = True, values = ["production", "component", "sdk"])},
)
