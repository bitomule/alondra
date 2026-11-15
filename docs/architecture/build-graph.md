# Build graph

Bazel only rebuilds and retests what an edit reaches, so the shape of the crate graph decides what
an edit costs. Rust has no early cutoff: a change to a private function body, or a test file that is
an input of a library, rebuilds every crate that depends on it, even when nothing public changed
([rustc's "relink, don't rebuild" goal](https://goals.rust-lang.org/2025h2/relink-dont-rebuild.html)
is not shipped). The graph is the only lever.

## Rules

- Dependencies point down. The binaries (`alondra`, `alondrad`) are the only crates that see
  everything; `alondrad` is where the daemon's pieces are put together.
- Code edited often lives in leaf crates; what everyone depends on stays small and rarely edited.
- A lower layer never names a higher one. When it has to reach up, it calls a trait it defines and
  the composition root hands it the implementation.
- A test target lives with the module it tests: one unit target per crate, and an integration target
  of its own when the crate needs real binaries. A name filter over one test binary is not a boundary.
- A library's inputs are its production sources. Test files belong only to test targets, so editing
  a test never rebuilds a dependent.
- A test declares everything it reads (see [running the tests](../running-tests.md#cached-results)),
  so a cached result can be trusted and `bazel test` alone selects what to run.

## Target graph

```
binaries:  alondra (CLI)            alondrad (composition root)
              │                        │
           views ◄─────────────────────┤
              │                        ├── engine-orchestrator ─┐
              │                        ├── engine-sdk ──────────┤ through traits of engine-kernel
              │                        ├── engine-admission ────┤
              │                        ├── engine-github, engine-mcp, engine-live, engine-worktrees
              │                        ▼
              │                  engine-kernel: store, snapshot, commit, change notices, traits
              ▼                        ▼
       domain rules (core::domain)   store   daemon-support
              ▼
          domain, foundation
```

`engine-kernel` keeps only the kernel's state: store, reads, snapshot, authority, live operations and
change notices. Each subsystem owns its own state, created and wired by `alondrad`, and hears of
commits through an observer trait instead of the kernel calling it.

## Where it starts (2026-11-15, main at bc73a80)

- `daemon-engine`: 54 of its 80 modules (38,853 of 44,386 lines without tests) form one cycle, so no
  crate can be taken out of it. `DomainRuntime` holds the state of most subsystems; 57 modules use it
  and it uses 19. With its outgoing edges inverted, 20 modules leave the cycle and 34 remain (SDK,
  admission and orchestrator).
- `core` (96k lines) sits under 7 crates; `store` under 11; `domain` under all of them.
- In the 319 commits since 2026-11-01: `daemon-engine` 123, `core` 118, `alondrad` 108, `alondra` 86.
  79 of the 89 commits that touch `core::domain` also touch the engine or the daemon.
- A line added to `core/src/views/tests.rs`, compiled only in test mode, rebuilt `core`,
  `daemon-support` and `daemon-engine-model`: the test file is an input of the production library.
- A fresh worktree built `daemon_engine_model` (945 actions) entirely from the disk and remote caches.

What each frequent edit reopens (`bazel query rdeps`, out of 173 test targets):

| Edit | Crates rebuilt | Test targets |
|---|---:|---:|
| `core/src/domain/rules.rs` | 16 | 40 |
| `core/src/views/tests.rs` (tests only) | 16 | 40 |
| `core/src/views/task_state.rs` | 16 | 40 |
| `domain/src/lifecycle.rs` | 23 | 59 |
| `store/src/lib.rs` | 20 | 50 |
| `daemon-support/src/disk.rs` | 14 | 35 |
| `daemon-engine/src/domain_runtime.rs` | 6 | 29 |
| `daemon-engine/src/admission.rs` | 6 | 29 |
| `daemon-engine/src/sdk_owner/tests.rs` (tests only) | 3 | 25 |
| `alondrad/src/views/actions.rs` | 4 | 25 |
| `alondra/src/mcp.rs` | 2 | 24 |
| `alondra/src/tui/tests/screens.rs` (tests only) | 2 | 24 |

The same edits through `bazel run //tools:edit_bench -- --query-only` once the repo-wide lints
declare the files they read (they now count as dependents, which they always were):

| Edit | Crates reopened | Test targets reopened | Actions run (rustc / clippy / tests) | Edit + test |
|---|---:|---:|---|---:|
| `crates/core/src/domain/rules.rs` | 20 | 41 | - | - |
| `crates/core/src/views/tests.rs` | 20 | 41 | - | - |
| `crates/core/src/domain/lifecycle.rs` | 20 | 41 | - | - |
| `crates/core/src/domain/lifecycle_tests.rs` | 20 | 41 | - | - |
| `crates/daemon-engine/src/sdk_owner/tests.rs` | 7 | 27 | - | - |
| `crates/core/src/domain/tests.rs` | 20 | 41 | - | - |
| `crates/daemon-engine/src/domain_runtime.rs` | 10 | 31 | - | - |
| `crates/daemon-engine/src/sdk_owner.rs` | 10 | 31 | - | - |
| `crates/core/src/domain/command.rs` | 20 | 41 | - | - |
| `crates/alondrad/src/views/actions.rs` | 8 | 27 | - | - |
| `crates/daemon-engine/src/sdk_owner/codex_tests.rs` | 7 | 27 | - | - |
| `crates/core/src/views/task_state.rs` | 20 | 41 | - | - |

EDIT_BENCH_TIMED
