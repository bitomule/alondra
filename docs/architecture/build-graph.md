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
change notices. Each subsystem owns its own state and hears of commits through an observer trait
instead of the kernel calling it.

## The kernel

`engine-kernel` holds `domain_runtime`, `domain_store`, the database migration, sessions, retention,
the settings projection and the capture of metrics. It depends on no engine subsystem, and only
`daemon-engine` and the subsystem crates may depend on it (Bazel visibility), so it can never import
one.

- A subsystem's state is an `Extension` (`domain_runtime/extensions.rs`): the runtime keeps one slot
  per extension type without naming the type. Each subsystem reads its state through a trait it
  defines and implements for the runtime (`LiveOf::live`, `SdkOwners::sdk_owners`,
  `HandoversOf::handovers`…), created on first use.
- A commit tells every `CommitObserver` (`domain_runtime/observers.rs`), in the order they were
  registered: the state readers are about to see, the change once the writer lock is released, and
  the command that made it. An observer may also carry out a command in place of the store, as the
  admission gate does with Resume on the card of a pause it holds.
- `daemon-engine/src/wiring.rs` puts the engine together: `wiring::open` opens the runtime with the
  decisions of the cards a node's mod holds and the observers of the SDK owners, the admission gate,
  the SDK nodes' waits, the task closer and «Allow always». The kernel's own constructor,
  `open_observed`, takes them as arguments; its tests open it with none.
- The kernel's tests are `//crates/engine-kernel:engine_kernel_test`. The few that need a subsystem
  (owner transactions, journal loading, commands racing retention, a node's turn read by its live
  session) run with the engine, under `daemon-engine/src/wiring/`.

`bazel test //tools:module_cycles_test` fails when the main cycle of a crate's production modules
grows past its limit in `tools/module_cycles.tsv`. `python3 tools/module_graph.py --production
<crate>/src` prints the graph it counts: the files `lib.rs` reaches through its `mod` declarations
(`#[path]` included), without `#[cfg(test)]` items, comments or literals. It only sees a dependency
written as a path (`crate::live::…`, an import): a method defined in another module and called on
the runtime used to be invisible to it.

| `daemon-engine` | Main cycle | Lines in it |
|---|---:|---:|
| Before (`main` at 16a6891) | 55 modules | 35,646 |
| Runtime without subsystems, as first measured | 34 modules | 28,654 |
| Kernel in its own crate | 45 modules | 32,053 |
| GitHub in its own crate | 42 modules | 30,333 |
| MCP, live nodes and worktrees in their own crates | 38 modules | 29,096 |

The 54 above counted modules by file name; the same graph through `#[path]` declarations holds one
more. The 34 missed what each module reached through methods the subsystems added to the runtime:
with the kernel in its own crate those are traits each caller imports, so the graph sees them, and
11 modules more depend on each other than that count showed (`domain.live()`, `domain.sdk()`, and
the engine's fixtures that open a wired runtime). Their dependencies were always there; they are
what phases 7 to 10 cut. The kernel's only cycle is `domain_runtime` with `domain_store`, inside the
crate.

## Subsystem crates

Each subsystem crate sits on `engine-kernel` beside the others: it names none of them, and only
`daemon-engine` may depend on it, which re-exports its modules so the engine and `alondrad` keep
their paths. Its tests open the runtime without the engine's observers; a test that needs another
subsystem runs with the engine. What a subsystem needs from one above it reaches it as a trait the
subsystem defines or as an argument.

- `engine-github`: reading pull requests with `gh` (`domain_github`), merging them
  (`domain_merges`), the images and viewed files of their descriptions (`pr_images`, `pr_viewed`),
  the sweep that adopts open PRs left without a live owner (`open_pr_sweep`) and the fake GitHub
  the tests and scenarios share (`github_world`, test hooks only). Its tests are
  `//crates/engine-github:engine_github_test`.
- `engine-mcp`: what a node waits for while an MCP server signs in (`mcp_waits`, `mcp_sign_in`)
  and the checks and deletions of the MCP servers screen (`mcp_probe`, `mcp_remove`). The SDK
  backend reaches it as `mcp_host::HostBackend`, which `daemon-engine` implements for `SdkBackend`.
- `engine-live`: what each node's mod reports or its Agent SDK or Codex host observes
  (`node_reports`, `node_work`, `sdk_live`, `codex_live`) and what each SDK task measured of its
  conversation (`contexts`). Its feeds take the node reports they write as an argument; the wait
  reports, which need the SDK backend's state folder, stay with the SDK owners
  (`sdk_owner/waits.rs`).
- `engine-worktrees`: each task's worktree (`task_worktrees`), those of cancelled tasks
  (`cancelled_worktrees`) and the startup moves of HOME files (`home_renames`), which now owns the
  names of the admission and orchestrator files it moves.

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

One edit timed end to end (`--edit crates/daemon-engine/src/sdk_owner/tests.rs`): a `#[used]`
static added to the engine's most edited test file rebuilds 7 crates and reruns all 27 test targets
it reaches, including the CLI's and the daemon's, because the CLI's log catalog reads the engine's
sources. Bazel's own time, without the queue's wait:

| Edit | Crates reopened | Test targets reopened | Actions run (rustc / clippy / tests) | Edit + test |
|---|---:|---:|---|---:|
| `crates/daemon-engine/src/sdk_owner/tests.rs` | 7 | 27 | 42 (7 / 3 / 31) | 374 s |
