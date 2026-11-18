# Crate layers

Every production Rust crate has one layer, and a dependency may only point at the layers its own is
allowed to use. The goal is the build: a crate that only the roots link is a leaf, so editing it
recompiles that crate, its test and the link of the root, not half the repository
([build-graph.md](build-graph.md)).

## The layers

| Layer | May depend on | Who sees it |
|---|---|---|
| `foundation` | foundation | everyone |
| `domain` | foundation, domain | every layer but foundation |
| `ports` | foundation, domain, ports | ports, application, adapters, roots |
| `application` | foundation, domain, ports | roots |
| `adapter-in` | foundation, domain, ports | roots |
| `adapter-out` | foundation, domain, ports | roots |
| `adapter-private` | foundation, domain, ports | only the adapter that owns it |
| `root` | foundation, domain, ports, application, adapters | nobody |

Pieces of the application never depend on each other, and adapters never depend on each other or on
the application: they talk through ports, and the roots wire them together.

## The crates

[`tools/bazel/crate_layers.bzl`](../../tools/bazel/crate_layers.bzl) is the table.

| Layer | Packages |
|---|---|
| foundation | `foundation`, `process-group`, `e2e`, `scenarios-transcript` |
| domain | `domain`, `domain-rules`, `core` (mixed: its accounts, launcher and update code goes to the `system` adapter) |
| ports | `engine-ports`, `provider-contract`, `view-model`, `store`, `agent-application` |
| application | `engine-kernel`, `daemon-engine`, `daemon-engine-model`, `daemon-engine-machine`, `engine-admission`, `engine-orchestrator`, `engine-sdk`, `engine-github`, `engine-live`, `engine-mcp`, `engine-worktrees`, `daemon-support` (mixed: most of it goes to the `system` adapter), `views` |
| adapter-in | none yet: `alondrad` and `alondra` hold theirs |
| adapter-out | `github-gh`, `provider-claude`, `provider-codex`, `daemon-engine-store`, `agent-runtime` |
| adapter-private | `agent-protocol` (of `provider-claude`), `codex-protocol` (of `provider-codex`) |
| root | `alondrad`, `alondra`, `relay`, `scenarios`, `tools/sandbox` |

`machine-model`, `provider-conformance`, the `*_test_hooks` libraries and other `testonly` targets
are tests: they have no layer and the rule does not apply to them, with one exception. A
`testonly` library of a package with a layer (its test hooks) may not link an output adapter that
the package's production crate may not use: every test that links those hooks would rerun on each
edit of the adapter. The fakes that need the adapter stay in the crate's own unit tests
(`#[cfg(test)]`), or fake the port instead. `TEST_HOOK_EXCEPTIONS` in the table lists the hooks that
still do, and only shrinks, like the exceptions below.

## Declaring a crate

A production crate is declared only with the macro of its layer, from
[`tools/bazel/crates.bzl`](../../tools/bazel/crates.bzl): `foundation_crate`, `domain_crate`,
`port_crate`, `application_crate`, `adapter_in_crate`, `adapter_out_crate`,
`adapter_private_crate`, `root_library` and `root_binary`. They take the same fixed parameters
(`name`, `srcs`, `deps`, `crate_name`, `crate_root`, `compile_data`, `version`, and
`test_visibility` for the packages whose tests use the crate). Each one:

- fails at loading when the package's layer in the table is another one;
- sets the crate's visibility to the `package_group` of each layer allowed to use it
  (`//tools/bazel:<layer>`), the dependents in the exception list and `test_visibility`;
- adds `<name>_layer_test`, whose analysis fails when the crate, or any crate below it, breaks the
  rule.

A new crate adds its package to the table and uses the macro. Unit tests and `testonly` libraries
are declared as before.

## What enforces it

- **Visibility.** A crate is visible only to the layers allowed to use it, so a forbidden dependency
  is a "target is not visible" error in any build.
- **`LayerInfo` and an aspect.** `<name>_layer_test` applies `layer_aspect` along `deps`: it gives
  every crate its `LayerInfo` and fails the analysis with the crate, both layers and the rule broken.
  It also covers a dependency that `test_visibility` lets through.
- **`//tools/bazel:crate_layers_test`.** A `genquery` reads every Rust target under `//:tree`. The
  test fails on a production `rust_library` or `rust_binary` whose `generator_function` is not a
  role macro, on a package with crates but no layer (or the reverse), on a forbidden dependency in
  any branch of a `select`, on test hooks that link an output adapter their crate may not use, and
  on an unused exception. It runs in MUSTS (`crate-layers`) and CI.

Why this combination, for Bazel 8.4:

- Legacy macros, not symbolic ones: a symbolic macro cannot call `glob()`, its targets'
  visibility is relative to the `.bzl` package, and its targets may not report the macro as
  `generator_function` ([macros](https://bazel.build/versions/8.4.2/extending/macros),
  [bazel#25705](https://github.com/bazelbuild/bazel/issues/25705)). A legacy macro is transparent
  to visibility and sets `generator_function`
  ([legacy macros](https://bazel.build/extending/legacy-macros)).
- Visibility is checked in every build at no cost, but its message only says "not visible"
  ([visibility](https://bazel.build/concepts/visibility)); the aspect names the rule broken.
- Nothing stops a BUILD file from loading `rust_library` from rules_rust: `visibility()` only
  restricts loading of this repository's own `.bzl` files. The census closes that gap.

## Exceptions

`LAYER_EXCEPTIONS` in the table lists the dependencies that break the rule today, by dependent
package: 62 when it was introduced. The list only shrinks: `crate_layers_test` fails on an entry
nothing uses, and on a count different from `EXCEPTION_CEILING` in
`tools/bazel/crate_layers_test.py`, which drops with every entry removed. Adding one means raising
that ceiling in the same change, in plain sight of review.

The phases of the build-graph plan empty it: `engine-ports` (phase 3) and the wide application
(phase 4) remove the application's dependencies on its sisters. Phase 3b moved `agent-runtime`
behind the `AgentHost` port of `engine-ports`: no provider or piece of the engine depends on it,
only the roots and the tests that start real hosts.
