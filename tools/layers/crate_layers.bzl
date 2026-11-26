"""The layer of every package that ships Rust, which layers each one may depend on, and the
dependencies that break the rule today (docs/architecture/layers.md).

The role macros in `crates.bzl` read this table: a package declares its crates with the macro of
its layer, the visibility of each crate lets only the layers above it in, and every crate's
`<name>_layer_test` fails the analysis on a dependency in a forbidden direction.
"""

FOUNDATION = "foundation"
DOMAIN = "domain"
PORTS = "ports"
APPLICATION = "application"
ADAPTER_IN = "adapter-in"
ADAPTER_OUT = "adapter-out"
ADAPTER_PRIVATE = "adapter-private"
ROOT = "root"

LAYERS = [FOUNDATION, DOMAIN, PORTS, APPLICATION, ADAPTER_IN, ADAPTER_OUT, ADAPTER_PRIVATE, ROOT]

_CORE = [FOUNDATION, DOMAIN, PORTS]

# The layers a crate of each layer may depend on. A crate always uses its own package. An
# `adapter-private` crate is used only by the adapter that owns it (`PRIVATE_TO`).
ALLOWED_DEPENDENCIES = {
    FOUNDATION: [FOUNDATION],
    DOMAIN: [FOUNDATION, DOMAIN],
    PORTS: _CORE,
    APPLICATION: _CORE,
    ADAPTER_IN: _CORE,
    ADAPTER_OUT: _CORE,
    ADAPTER_PRIVATE: _CORE,
    ROOT: _CORE + [APPLICATION, ADAPTER_IN, ADAPTER_OUT],
}

CRATE_LAYERS = {
    "crates/foundation": FOUNDATION,
    "crates/process-group": FOUNDATION,
    "crates/host": FOUNDATION,
    "crates/e2e": FOUNDATION,
    "crates/scenarios-transcript": FOUNDATION,
    "crates/spec-engine": FOUNDATION,
    "crates/domain": DOMAIN,
    "crates/domain-model": DOMAIN,
    # Mixed: its config, options and home lock still read and write Alondra's home.
    "crates/core": DOMAIN,
    "crates/provider-contract": PORTS,
    "crates/engine-ports": PORTS,
    "crates/view-model": PORTS,
    "crates/client-port": PORTS,
    "crates/store": PORTS,
    "crates/agent-application": PORTS,
    "crates/engine-kernel": APPLICATION,
    "crates/daemon-engine": APPLICATION,
    "crates/daemon-engine-model": APPLICATION,
    "crates/daemon-engine-machine": APPLICATION,
    "crates/engine-admission": APPLICATION,
    "crates/engine-orchestrator": APPLICATION,
    "crates/engine-sdk": APPLICATION,
    "crates/engine-github": APPLICATION,
    "crates/engine-live": APPLICATION,
    "crates/engine-mcp": APPLICATION,
    "crates/engine-worktrees": APPLICATION,
    # Mixed: most of it is git, disk and process code that moves to the `system` adapter.
    "crates/daemon-support": APPLICATION,
    "crates/views": APPLICATION,
    "crates/github-gh": ADAPTER_OUT,
    "crates/cli": ADAPTER_IN,
    "crates/cli-mcp": ADAPTER_IN,
    "crates/cli-report": ADAPTER_IN,
    "crates/cli-tui": ADAPTER_IN,
    "crates/client-transport": ADAPTER_IN,
    "crates/provider-antigravity": ADAPTER_OUT,
    "crates/provider-claude": ADAPTER_OUT,
    "crates/provider-codex": ADAPTER_OUT,
    "crates/store-sqlite": ADAPTER_OUT,
    "crates/system": ADAPTER_OUT,
    "crates/agent-runtime": ADAPTER_OUT,
    "crates/agent-protocol": ADAPTER_PRIVATE,
    "crates/antigravity-protocol": ADAPTER_PRIVATE,
    "crates/codex-protocol": ADAPTER_PRIVATE,
    "crates/daemon-server": ADAPTER_IN,
    "crates/alondrad": ROOT,
    "crates/alondra": ROOT,
    "crates/relay": ROOT,
    "crates/scenarios": ROOT,
    "tools/sandbox": ROOT,
}

PRIVATE_TO = {
    "crates/agent-protocol": "crates/provider-claude",
    "crates/antigravity-protocol": "crates/provider-antigravity",
    "crates/codex-protocol": "crates/provider-codex",
}

# The dependencies that break the rule today, by dependent package. This list only shrinks: a
# change that removes a dependency removes its entry, `crate_layers_test` fails on an entry no
# dependency uses, and its ceiling drops with every entry removed.
LAYER_EXCEPTIONS = {
    "crates/daemon-engine": [
        "crates/daemon-engine-machine",
        "crates/daemon-engine-model",
        "crates/daemon-support",
        "crates/engine-github",
        "crates/engine-kernel",
        "crates/engine-live",
        "crates/engine-mcp",
        "crates/engine-worktrees",
        "crates/provider-claude",
        "crates/provider-codex",
    ],
    "crates/daemon-engine-machine": [
        "crates/daemon-engine-model",
        "crates/daemon-support",
    ],
    "crates/daemon-engine-model": [
        "crates/daemon-support",
    ],
    "crates/store-sqlite": [
        "crates/daemon-engine-model",
        "crates/daemon-support",
        "crates/provider-codex",
    ],
    # The server reaches the engine directly until phase 4 puts the application behind ports.
    "crates/daemon-server": [
        "crates/daemon-engine",
        "crates/daemon-support",
        "crates/engine-admission",
        "crates/engine-orchestrator",
        "crates/engine-sdk",
    ],
    "crates/daemon-support": [
        "crates/provider-claude",
    ],
    "crates/engine-admission": [
        "crates/daemon-engine-machine",
        "crates/daemon-engine-model",
        "crates/daemon-support",
    ],
    "crates/engine-github": [
        "crates/daemon-engine-model",
        "crates/daemon-support",
        "crates/engine-kernel",
    ],
    "crates/engine-kernel": [
        "crates/daemon-engine-machine",
        "crates/daemon-engine-model",
        "crates/daemon-support",
    ],
    "crates/engine-live": [
        "crates/daemon-support",
        "crates/engine-kernel",
        "crates/provider-claude",
        "crates/provider-codex",
    ],
    "crates/engine-mcp": [
        "crates/daemon-support",
        "crates/engine-kernel",
        "crates/provider-claude",
    ],
    "crates/engine-orchestrator": [
        "crates/daemon-engine-model",
        "crates/daemon-support",
        "crates/engine-kernel",
        "crates/provider-claude",
        "crates/provider-codex",
    ],
    "crates/engine-sdk": [
        "crates/daemon-engine-machine",
        "crates/daemon-engine-model",
        "crates/daemon-support",
        "crates/engine-github",
        "crates/engine-kernel",
        "crates/provider-antigravity",
        "crates/provider-claude",
        "crates/provider-codex",
    ],
    "crates/engine-worktrees": [
        "crates/daemon-support",
        "crates/engine-kernel",
    ],
}

# The packages outside the root whose tests run a crate or a binary of the root: they rerun on
# every edit of the daemon. Any other test that reaches the root, through `deps` or `data`, fails
# `crate_layers_test`; this list only shrinks.
ROOT_TEST_CONSUMERS = [
    # The sandbox journeys, run on request against the binaries.
    "tests/sandbox",
    # The binaries on an empty home.
    "tests/system/macos",
]

def layer_violation(package, dependency):
    """Why `package` may not depend on `dependency`, or None when it may."""
    if package == dependency or dependency in LAYER_EXCEPTIONS.get(package, []):
        return None
    layer = CRATE_LAYERS[package]
    dependency_layer = CRATE_LAYERS[dependency]
    if dependency_layer == ADAPTER_PRIVATE:
        if PRIVATE_TO[dependency] == package:
            return None
        return "{} is private to the adapter {}, and only it may depend on it.".format(dependency, PRIVATE_TO[dependency])
    if dependency_layer in ALLOWED_DEPENDENCIES[layer]:
        return None
    return "A crate of layer {} may depend only on layers {}.".format(layer, ", ".join(ALLOWED_DEPENDENCIES[layer]))
