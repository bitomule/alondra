"""Validate production and test source boundaries on actual Rustc actions."""

load("@bazel_skylib//lib:unittest.bzl", "analysistest", "asserts")

def _rust_inputs_test_impl(ctx):
    env = analysistest.begin(ctx)
    actions = [action for action in analysistest.target_actions(env) if action.mnemonic in ["Rustc", "RustcMetadata"]]
    asserts.true(env, len(actions) > 0, "The target must register an actual Rust compile action")
    for action in actions:
        inputs = {source.short_path: True for source in action.inputs.to_list()}
        for source in ["crates/alondrad/src/lib.rs", "crates/alondrad/src/domain_store/owner.rs"]:
            asserts.true(env, source in inputs, "{} must compile {}".format(action.mnemonic, source))
        test_inputs = [
            source
            for source in inputs
            if source == "crates/test_support.rs" or (source.startswith("crates/alondrad/src/") and (
                source.endswith("_tests.rs") or source.endswith("/tests.rs") or "/tests/" in source
            ))
        ]
        for source in ["crates/alondrad/src/domain_store/owner_tests.rs", "crates/alondrad/src/service/tests/iroh_tests.rs", "crates/test_support.rs"]:
            asserts.equals(
                env,
                ctx.attr.test_sources,
                source in inputs,
                "{} input {} must have test-only presence {}".format(action.mnemonic, source, ctx.attr.test_sources),
            )
        if not ctx.attr.test_sources:
            asserts.equals(env, [], sorted(test_inputs), "{} must not compile test files".format(action.mnemonic))
    return analysistest.end(env)

rust_inputs_test = analysistest.make(
    _rust_inputs_test_impl,
    attrs = {"test_sources": attr.bool(mandatory = True)},
)
