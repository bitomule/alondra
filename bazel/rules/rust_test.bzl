"""rust_test compiled at a low opt-level: a test crate that recompiles a whole library at -c opt needs
several times the memory and time of one at opt-level 1, and its speed is not what a test measures."""

load("@rules_rust//rust:defs.bzl", _rust_test = "rust_test")
load(":rust.bzl", "incremental")

LOW_OPT_LEVEL = "-Copt-level=0"

def low_opt_rust_test(rustc_flags = [], **kwargs):
    _rust_test(rustc_flags = rustc_flags + [LOW_OPT_LEVEL], **kwargs)

def incremental_rust_test(**kwargs):
    """A low_opt_rust_test that keeps rustc's incremental state and is never cached, as rust.bzl's
    libraries: for a test crate that recompiles a large library on every edit below it, where the
    state pays for its disk (docs/architecture/build-graph.md)."""
    low_opt_rust_test(**incremental(kwargs))
