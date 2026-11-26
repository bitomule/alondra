"""rustc must not rewrite what it links.

With `-Cstrip` other than `none`, rustc runs rust-objcopy over the linked file and only warns when it
fails. On a full disk rust-objcopy leaves only the last `size % 4096` bytes of the binary there, rustc
exits 0 and Bazel keeps that as the output. The linker leaves the debug information out itself
(`ld -S`), and its failures are errors.
"""

load("@bazel_skylib//lib:unittest.bzl", "analysistest", "asserts")

def _rust_link_test_impl(ctx):
    env = analysistest.begin(ctx)
    actions = [action for action in analysistest.target_actions(env) if action.mnemonic == "Rustc"]
    asserts.true(env, len(actions) > 0, "The target must register an actual Rust compile action")
    for action in actions:
        strip = "none"
        for argument in action.argv:
            for prefix in ["--codegen=strip=", "-Cstrip="]:
                if argument.startswith(prefix):
                    strip = argument.removeprefix(prefix)
        asserts.equals(env, "none", strip, "rustc must not strip what it links: a failed strip is only a warning")
        asserts.true(env, "-Clink-arg=-Wl,-S" in action.argv, "The linker must leave the debug information out")
    return analysistest.end(env)

rust_link_test = analysistest.make(_rust_link_test_impl)
