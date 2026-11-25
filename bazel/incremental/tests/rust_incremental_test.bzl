"""What rust.bzl gives a crate: rustc's incremental state, unless the switch is off. `no-cache`, which
analysis cannot see, is checked by //tools/layers:crate_layers_test."""

load("@bazel_skylib//lib:unittest.bzl", "analysistest", "asserts")
load("//bazel/rules:rust.bzl", "INCREMENTAL_STATE")

def _rustc(env):
    return [action for action in analysistest.target_actions(env) if action.mnemonic == "Rustc"][0]

def _incremental_flags(action):
    return [argument for argument in action.argv if argument.startswith("-Cincremental=")]

def _compiles_incrementally_impl(ctx):
    env = analysistest.begin(ctx)
    rustc = _rustc(env)
    asserts.equals(env, ["-Cincremental=" + INCREMENTAL_STATE], _incremental_flags(rustc))
    return analysistest.end(env)

compiles_incrementally_test = analysistest.make(_compiles_incrementally_impl)

def _compiles_from_scratch_impl(ctx):
    env = analysistest.begin(ctx)
    rustc = _rustc(env)
    asserts.equals(env, [], _incremental_flags(rustc))
    return analysistest.end(env)

compiles_from_scratch_test = analysistest.make(
    _compiles_from_scratch_impl,
    config_settings = {str(Label("//bazel/incremental")): False},
)
