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

[`tools/layers/crate_layers.bzl`](../../tools/layers/crate_layers.bzl) is the table.

| Layer | Packages |
|---|---|
| foundation | `foundation` (with the clock, the daemon's log, its locks and atomic saves), `process-group`, `host` (Bonjour, the Claude Code install and its updater, the record of failed starts, the Homebrew install and the user the process runs as: what the CLI, the server and the clients reach by themselves), `e2e`, `scenarios-transcript`, `spec-engine` (specs, boundaries, decisions and what is validated, read from the files its caller hands it) |
| domain | `domain`, `domain-rules`, `core` (mixed: its config, options and home lock still read and write Alondra's home) |
| ports | `engine-ports`, `provider-contract`, `view-model`, `client-port`, `store`, `agent-application` |
| application | `engine-kernel`, `daemon-engine`, `daemon-engine-model`, `daemon-engine-machine`, `engine-admission`, `engine-orchestrator`, `engine-sdk`, `engine-github`, `engine-live`, `engine-mcp`, `engine-worktrees`, `daemon-support` (mixed: most of it goes to the `system` adapter), `views` |
| adapter-in | `daemon-server` (the daemon's routes, transports, pushes and screens' cache), `cli`, `cli-tui`, `cli-mcp`, `cli-report`, `client-transport` |
| adapter-out | `github-gh`, `provider-antigravity`, `provider-claude`, `provider-codex`, `store-sqlite`, `agent-runtime`, `system` (git, the task worktrees, the disks, the provider accounts, the processes, Alondra's plugins and the MCP sign-in listeners, behind the `Worktrees`, `Disk`, `Accounts`, `Processes`, `Plugins` and `SignInReturns` ports) |
| adapter-private | `agent-protocol` (of `provider-claude`), `antigravity-protocol` (of `provider-antigravity`), `codex-protocol` (of `provider-codex`) |
| root | `alondrad`, `alondra`, `relay`, `scenarios`, `tools/sandbox` |

`machine-model`, `provider-conformance`, `engine-doubles` and other `testonly` targets are tests:
they have no layer and the rule does not apply to them, with one exception. No `testonly` library
builds a production crate a second time, as the `test-hooks` builds did: every crate between it and
its tests would compile twice. A test registers the doubles it needs in the runtime it opens, and
what only tests change is set at run time.

A test has the layer of its package too. A test outside the root may not depend on a crate or a
binary of the root, through `deps` or `data` at any depth: it would rerun on every edit of every
crate the daemon links, whatever it checks. It tests against a port or its double, or it lives in
the root. `ROOT_TEST_CONSUMERS` in the table lists the packages whose tests run the binaries (the
system and sandbox suites), and only shrinks. The tests of the real `alondrad` and `alondra` are
`//crates/alondrad:binary_smoke_test`.

## Declaring a crate

A production crate is declared only with the macro of its layer, from
[`tools/layers/crates.bzl`](../../tools/layers/crates.bzl): `foundation_crate`, `domain_crate`,
`port_crate`, `application_crate`, `adapter_in_crate`, `adapter_out_crate`,
`adapter_private_crate`, `root_library` and `root_binary`. They take the same fixed parameters
(`name`, `srcs`, `deps`, `crate_name`, `crate_root`, `compile_data`, `version`, and
`test_visibility` for the packages whose tests use the crate). Each one:

- fails at loading when the package's layer in the table is another one;
- sets the crate's visibility to the `package_group` of each layer allowed to use it
  (`//tools/layers:<layer>`), the dependents in the exception list and `test_visibility`;
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
- **`//tools/layers:crate_layers_test`.** A `genquery` reads every Rust target under `//:tree`. The
  test fails on a production `rust_library` or `rust_binary` whose `generator_function` is not a
  role macro, on a package with crates but no layer (or the reverse), on a forbidden dependency in
  any branch of a `select`, on a `testonly` library that builds a production crate again, on an
  unused exception, and on a test outside the root that reaches a crate or binary of the root (a
  second `genquery` gives every rule with what it reads, and the test follows them). It runs in
  MUSTS (`crate-layers`) and CI.

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
`tools/layers/crate_layers_test.py`, which drops with every entry removed. Adding one means raising
that ceiling in the same change, in plain sight of review.

The phases of the build-graph plan empty it: `engine-ports` (phase 3) and the wide application
(phase 4) remove the application's dependencies on its sisters, and the five of `daemon-server` on
the engine, which phase 6 added when it took the server out of the root (a decision of the user's
while the engine is not yet behind ports). Its sixth, on `github-gh`, is gone: the server takes
GitHub from the registry of ports, and only `alondrad_production`, the library of the `alondrad`
binary, links the real client. Phase 3b moved `agent-runtime`
behind the `AgentHost` port of `engine-ports`: no provider or piece of the engine depends on it,
only the roots and the tests that start real hosts.

`engine-sdk` reaches `provider-antigravity` the way it reaches the Claude and Codex providers, for
its `Worker::Antigravity`: the one entry Antigravity nodes added, with the ceiling raised from 53 to
54. Nothing else of the engine names the Antigravity crates: `engine-sdk` reads the host's frames
and passes agy's messages to the live view through `SdkEngine::feed_antigravity`, so `engine-live`
and `daemon-engine` read plain JSON.
