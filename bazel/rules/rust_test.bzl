"""rust_test compiled at a low opt-level: a test crate that recompiles a whole library at -c opt needs
several times the memory and time of one at opt-level 1, and its speed is not what a test measures."""

load("@rules_rust//rust:defs.bzl", _rust_test = "rust_test")

LOW_OPT_LEVEL = "-Copt-level=0"

def low_opt_rust_test(rustc_flags = [], **kwargs):
    _rust_test(rustc_flags = rustc_flags + [LOW_OPT_LEVEL], **kwargs)
