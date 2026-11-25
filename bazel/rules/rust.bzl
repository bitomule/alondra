"""The rules this repository declares its libraries and binaries with: rules_rust's, compiled incrementally.

An edit recompiles only what it changed in each crate it reaches, instead of every crate above it from
scratch (docs/architecture/build-graph.md). The state lives in the output base, two levels above the
unsandboxed execroot, and goes with it. `--//bazel/incremental=false` (release and CI) compiles
from scratch. An incremental result never enters a cache: an rlib its state left incoherent would
reach every checkout with the same sources, so these targets carry `no-cache`. Test crates compile
from scratch and stay cached: incremental, they would take 8 GB more per checkout for about 25 s per
edit of the domain. Third-party crates keep rules_rust's rules and their cache.
"""

load(
    "@rules_rust//rust:defs.bzl",
    _rust_binary = "rust_binary",
    _rust_library = "rust_library",
)

INCREMENTAL_STATE = "../../rustc-incremental"
NO_CACHE = "no-cache"

def _incremental(kwargs):
    kwargs["rustc_flags"] = (kwargs.get("rustc_flags") or []) + select({
        Label("//bazel/incremental:on"): ["-Cincremental=" + INCREMENTAL_STATE],
        "//conditions:default": [],
    })
    kwargs["tags"] = (kwargs.get("tags") or []) + [NO_CACHE]
    return kwargs

def rust_library(**kwargs):
    _rust_library(**_incremental(kwargs))

def rust_binary(**kwargs):
    _rust_binary(**_incremental(kwargs))
