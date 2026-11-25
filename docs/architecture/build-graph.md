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
  the command that made it. An observer may also carry out a command in place of the store, as
  `daemon-engine` does with Resume on the card of a pause the admission gate holds, which it hands to
  the gate.
- `engine-sdk/src/wiring.rs` puts the engine together: `wiring::open` opens the runtime with the
  decisions of the cards a node's mod holds and the observers of the admission gate's pauses, the
  SDK nodes' waits, the task closer, «Allow always» and the SDK owners, in that order, and gives it
  the ports the crates of the engine ask each other through. The kernel's own constructor,
  `open_observed`, takes them as arguments; its tests open it with none. `daemon-engine/src/wiring.rs`
  keeps what the orchestrator asks of the engine and, for the engine's tests, a runtime with only
  the engine's own observers (`TEST_OPEN` lets a crate above open the whole daemon in its place).
- The kernel's tests are `//crates/engine-kernel:engine_kernel_test`. The few that need a subsystem
  (owner transactions, journal loading, commands racing retention, a node's turn read by its live
  session) run with the engine, under `daemon-engine/src/wiring/`, or with the SDK's crate when
  they need its owners.

`bazel test //tools:module_cycles_test` fails when the main cycle of a crate's production modules
grows past its limit in `tools/module_cycles.tsv`. `python3 tools/module_graph.py --production
<crate>/src` prints the graph it counts: the files `lib.rs` reaches through its `mod` declarations
(`#[path]` included), without comments, literals or what only tests or the `test-hooks` feature
compile (`#[cfg(test)]`, `#[cfg(any(test, feature = "test-hooks"))]`), down to a single field. It
only sees a dependency written as a path (`crate::live::…`, an import): a method defined in another
module and called on the runtime used to be invisible to it.

| `daemon-engine` | Main cycle | Lines in it |
|---|---:|---:|
| Before (`main` at 16a6891) | 55 modules | 35,646 |
| Runtime without subsystems, as first measured | 34 modules | 28,654 |
| Kernel in its own crate | 45 modules | 32,053 |
| GitHub in its own crate | 42 modules | 30,333 |
| The same, without what only the `test-hooks` feature compiles (`main` at 70324de) | 39 modules | 17,312 |
| Orchestrator in its own crate | 31 modules | 14,023 |
| Admission gate in its own crate | 26 modules | 11,640 |
| The SDK and the launching of tasks in their own crate | 18 modules | 5,489 |
| MCP, live nodes and worktrees in their own crates | 13 modules | 4,071 |

The 54 above counted modules by file name; the same graph through `#[path]` declarations holds one
more. The 34 missed what each module reached through methods the subsystems added to the runtime:
with the kernel in its own crate those are traits each caller imports, so the graph sees them, and
11 modules more depend on each other than that count showed (`domain.live()`, `domain.sdk()`, and
the engine's fixtures that open a wired runtime). Their dependencies were always there; they are
what phases 7 to 10 cut. The kernel's only cycle was `domain_runtime` with `domain_store`, inside the
crate.

Until the orchestrator's crate the graph also counted what only the `test-hooks` feature compiles
(`sdk_owner::tests`, `domain_orchestrator_tools::tests`, the fixtures of `domain_store`…), about half
the lines it reported, and a `#[cfg(test)]` field hid the rest of its file. Without them the
kernel has no cycle and the engine's main cycle measures 39 modules on `main` at 70324de.

## The ports

`engine-ports` holds the ports the pieces of the engine call each other through, the values they
speak in, and the registry that holds one implementation of each. It depends only on `core` and
`domain-rules`, and on `foundation` and `process-group` below them: every piece depends on it, and
none on the piece that implements a port it uses.

- The ports: `AdmissionGate` and `AdmissionEngine` (`admission.rs`), `OrchestratorEngine` and
  `OrchestratorAnswers` (`orchestrator.rs`), `SdkHosts` (`hosts.rs`), `SdkEngine` (`sdk.rs`), `AgentHost` (`agent_host.rs`) and `GitHub` (`github.rs`, see "Output adapters"). The values they carry moved with them:
  `LaunchOptions` and `Failed` (`launch.rs`), `NoRoom` (`machine.rs`) and `OutboxMessage`
  (`outbox.rs`). The crates they came from re-export them, so their callers did not change.
- `Journal` (`journal.rs`) is how the runtime speaks to where the domain is kept: the writer that
  commits commands and the engine's own records, the reader the screens use, and the owner
  transaction (`OwnerJournal`) that stages a provider's observations with the domain's commands.
  The runtime holds them as `Box<dyn Journal>` and never names the store behind them;
  `JournalFiles` opens them and migrates or checks the database on disk. The SQLite adapter,
  `store-sqlite`, implements both (`alondra_store_sqlite::Sqlite`), and only `alondrad` names it: it
  hands it to the opening of the runtime and to the preflight. The runtime opens its journal before
  any `wire` runs, so it is given to `DomainRuntime::open_observed` rather than asked of the
  registry. The values the journal carries moved with it (`StoreError`, `Commit`, `TaskClosing`,
  the handover, account move and provider transition records, the batch reports…), and the store
  re-exports them, so its own code did not change. A test that reads the store's tables reaches the
  SQLite store behind the port with `AsSqlite` (`journal.sqlite().conn`).
- The registry (`Registry`) maps each port's type to its implementation: `provide::<dyn P>(…)`
  gives it, `port::<dyn P>()` asks for it, and `missing()` names every port nothing implements. The
  runtime holds one (`domain.ports()`).
- Each piece that implements a port exposes `pub fn wire(&Registry)`: `daemon-engine`
  (`wiring::wire`), `engine-sdk` (`wiring::wire`), `engine-admission`, `engine-orchestrator` and
  the adapter `github-gh`.
  `alondrad` calls all of them (`domain_bootstrap::wire`), and its test
  `the_daemon_implements_every_port` fails when a port is left without an implementation.
- Every port is called on a `&dyn Runtime`, the context the engine keeps its state in, as
  rust-analyzer's queries take `db: &dyn HirDatabase`. The trait (`engine-ports/src/runtime.rs`)
  says what a piece may ask of the kernel: its home, the committed state and the live read with its
  index, the journal with its launch queue, the subscriptions to revisions, snapshots and schedules,
  a command as Alondra itself or as an authorized actor, an owner transaction, the seals of a task's
  observations, the registry, a task's launch plan and the extensions (`extensions()`: the state
  each piece keeps in the runtime, by type, in `engine-ports/src/extensions.rs`). The cards Alondra
  raises and withdraws (`raise_problem`, `withdraw_problem`, `tell_orchestrator`, `ask_user`…) are
  methods of the trait built on its system commands. `DomainRuntime` implements it, and no piece is
  written against `DomainRuntime`: what each keeps in the runtime it reaches through an extension
  trait implemented for every `Runtime` (`LiveOf`, `SdkOf`, `OrchestratorOf`, `RuntimeRecords`…).
  The test double `engine_doubles::runtime::Forwarded`, a runtime that is not the kernel's, shows a
  port runs on any `Runtime`. Generics would compile each piece into its callers and stop the ports
  being `dyn` in the registry.
- `AgentHost` (`agent_host.rs`) is how the providers and the engine start, adopt, stop and reclaim
  the processes their agents run in, and run a provider's command line to its end. `agent-runtime`
  implements it (`ProcessHosts`) and `alondra_agent_runtime::wire(&Registry, launcher)` provides it:
  `alondrad` with the launcher of its seams, so a scenario's stand-in host takes the real one's place.
  The providers get it in their configuration; the engine asks the registry
  (`agent_host::of(domain.ports())`). The values it carries moved with it (`VerifiedArtifact`,
  `LaunchIdentity`, `HostStop`, `EndedHost`…), and so did the owner wrappers `OwnedHost` and
  `AdoptedHost`. The file digest and the socket poll they need are plain computations, in
  `foundation`.

## The orchestrator

`engine-orchestrator` holds the process that runs the orchestrator agent: `domain_orchestrator` with
its recovery and watchdog, `orchestrator_inbox`, `orchestrator_process`, `orchestrator_provider`,
`orchestrator_sdk`, `orchestrator_codex` and `stall_context`. It depends on `engine-kernel` and the
crates below it, never on `daemon-engine`, and nothing but `alondrad` depends on it (Bazel
visibility on both).

- The two crates call each other through ports, traits of `engine-ports` (see "The ports" below).
  `OrchestratorEngine` is what the orchestrator asks of the engine (choose an account, the usage
  pause, the account wait, the obligation clock, a message's text, a node's last words);
  `alondra_daemon_engine::wiring::wire` provides it. `OrchestratorAnswers` is what the obligation
  clock asks of the orchestrator; `alondra_engine_orchestrator::wire` provides it.
- What both read and is only data moved below them: the SDK backend (`engine-kernel/src/sdk_backend.rs`,
  with `domain.sdk()`), the account wait and the usage pause's value (`engine-ports/src/orchestrator.rs`), the count of the
  orchestrator's rests (a field of the runtime) and `auth::uuid` (`daemon-support`).
- The orchestrator's own state (its host, its stop, its runs) is an extension of its crate
  (`engine.rs`), no longer a field of the engine's `Live`.
- Its tests are `//crates/engine-orchestrator:engine_orchestrator_test`, on the doubled hosts the
  orchestrator model of `//crates/provider-claude:claude_host_model` plays as the fake orchestrator
  Claude plays them, with no program run, and `engine_orchestrator_host_test` (the feature
  `real-hosts`), with the real hosts, fake Claude and Codex and processes the rest still need. They
  link neither `daemon-engine` nor `engine-sdk`: they
  open a runtime with the doubles of `engine-doubles` registered for the ports they do not test. The
  tests that need the whole engine are in `alondrad` (`engine_tests/orchestrator_*_tests.rs`).
- What only tests changed is set at run time on the orchestrator's extension (`Injected`): the
  failed writes, and hooks on a batch's spawn and drain. There is no `test-hooks` build of the crate.
- The obligation clock (`OrchestratorAnswers::settle_answers`) works on the `&dyn Runtime` alone:
  `the_clock_settles_what_the_orchestrator_owes_on_any_runtime` fails when it is taken for the
  kernel's again.

## The admission gate

`engine-admission` holds the gate every start, resume and turn passes through (`admission`), the
pauses it holds nodes in (`pauses`) and the watch over the Mac's memory and disk it decides on
(`machine_watch`). It only decides: the gauges come from the `MachineProbe` port, which `system`
reads from the kernel. It depends on `engine-kernel` and the crates below it, never on `daemon-engine`,
and only `alondrad`, the machine model and the scenarios see it (Bazel visibility).

- `daemon-engine` asks the gate through the port `AdmissionGate` (`engine-ports/src/admission.rs`):
  ask for a start, take a turn, hold back or resume a node, the pauses it keeps, the place of a queued
  task and the disk's room for a launch. `domain.gate()` (`daemon-engine/src/gate.rs`) is that port
  for one runtime; `alondra_engine_admission::wire` provides it.
- What the port speaks in is only data and moved below both crates, with the port: what waits at the
  gate (`Key`, `Waiting`), why it waits (`Hold`), the pauses (`Reason`, `Paused`, `Pauses`) and two
  reads of the domain's state (`open_task_of`, `run_priority`). The record of the usage pause stays
  in `daemon-engine` (`usage_pause`): the usage probe and the accounts keep it, and only one of its
  calls reaches the gate.
- The gate asks the engine through the port `AdmissionEngine`, which `daemon-engine` provides: tell a
  node, the handover, a node's events, a failed start, a launch and the reads of `Live`. It asks the
  hosts' owner through `SdkHosts`. What it used that is pure moved to `engine-ports`: the API watch
  (`api.rs`), the usage clock's text and `Alerts` (`usage.rs`), `candidate` and `ALONDRA`.
- The gate and the machine watch are extensions of `engine-admission` instead of fields of `Live`.
  The pacing that only tests changed is set at run time (`Admission::tune`, `Tuning`).
- Its tests are `//crates/engine-admission:engine_admission_test`, against the doubles of
  `engine-doubles` (`admission::Engine`, `hosts::NoHosts`). The ones that need the whole engine are
  in `alondrad` (`engine_tests/admission_tests.rs`). The engine's own test binary does not link the
  gate: a runtime without one lets every start and turn through.
- The test binaries that need the gate (`alondrad`, `engine-sdk`, the machine model) link the
  production `engine_admission` and register it with `engine_admission::for_tests`.

## The SDK and the launching of tasks

`engine-sdk` is the top of the engine. It holds the owners of the Agent SDK hosts (`sdk_owner`), the
moves of a task to another provider or account (`provider_transition`, `account_move`) and of the
`claude --bg` nodes an earlier release left (`bg_handover`), the automatic launch and its retries
(`auto_launch`, `start_retries`), the scheduled runs (`domain_schedule_runner`,
`domain_schedule_api`), the closing of tasks and the release of resting hosts (`task_closer`,
`idle_release`) and its own wiring, which provides `SdkHosts`. Its code asks the rest of the engine
only through the port `SdkEngine` (see "`engine-sdk` asks the engine through `SdkEngine`"); only
`alondrad`, the machine model and the scenarios see it. `alondrad` puts the engine together
(`alondrad/src/wiring.rs`).

- The engine and the gate ask it through the port `SdkHosts` (`daemon-engine/src/hosts.rs`): whether
  a node's host runs, stop or halt it, take over the tasks no owner runs, check and prepare the
  reopening of a session, launch a node, and whether a task moves to another provider.
  `domain.hosts()` is that port for one runtime; `engine-sdk`'s wiring provides it.
- What is only data or a read of the domain's state moved down instead: the launch itself
  (`hosts::Launch`), which tasks run through the SDK (`sdk_live::runs_through_sdk`), which tasks the
  automatic launch may start (`launch_plans::candidate`), the text a node starts with, the wake of a
  node whose input was not taken (`untaken`) and the record of a failed start (`start_failures`).
- `sdk_handover` keeps in the engine what reaches a `claude --bg` node that is moving and how Alondra
  stops one; the moves themselves are `bg_handover`.
- The SDK's tests run in three targets of the same sources, split by the features `whole-engine`
  and `real-hosts` (see "`engine-sdk`'s tests without the engine"):
  `//crates/engine-sdk:engine_sdk_test` runs the crate's own pieces on the doubles of the rest of
  the engine and links neither `daemon-engine` nor `engine-admission`;
  `//crates/engine-sdk:engine_sdk_engine_test` opens the whole engine on the doubled hosts with the
  Claude host model and carries no host executable; `//crates/engine-sdk:engine_sdk_host_test`
  compiles only the tests that need the real Node or Codex host or a fake native Claude or Codex
  (see "The SDK's tests on the double" and "The owner's tests on the model"). None carries a binary of the root (see "Tests leave the
  root"), and no test runs on a name filter.

## Output adapters

An output adapter implements a port of `engine-ports` against the world outside the daemon. It
depends only on ports, the domain and foundation, and no production crate but the roots sees it. A
subsystem's test hooks may still link it for their fakes, and then an edit reopens the tests above
them too (see "Output adapter `github-gh`" below).

- `github-gh`: GitHub through the Mac's `gh` (`GhCli`). It implements every trait of the `GitHub`
  port (`PrObserver`, `PrMerger`, `GitHubImages`, `Gh`), parses what `gh` answers and reads the
  viewed files of a PR (`pr_viewed`). `wire` gives the registry `github()`; `alondrad`'s assembly
  replaces it with its seams' GitHub, which a scenario fills with `GithubWorld`. `engine-github`,
  `engine-kernel` (PR sizes) and `daemon-engine` (a branch's PR) ask the registry for it.

## Subsystem crates

Each subsystem crate sits on `engine-kernel` beside the others: it names none of them, and only
`daemon-engine` may depend on it, which re-exports its modules so the engine and `alondrad` keep
their paths. Its tests open the runtime with `DomainRuntime::open_unwired`, without the engine's
observers; a test that needs another subsystem runs with the engine.

- `engine-github`: what Alondra decides from GitHub's pull requests (`domain_github`), merging
  them (`domain_merges`), the images of their descriptions (`pr_images`), the sweep that adopts open PRs left without a live owner (`open_pr_sweep`) and the fake GitHub
  the tests and scenarios share (`github_world`, test hooks only). Its tests are
  `//crates/engine-github:engine_github_test`.
- `engine-mcp`: what a node waits for while an MCP server signs in (`mcp_waits`, `mcp_sign_in`)
  and the checks and deletions of the MCP servers screen (`mcp_probe`, `mcp_remove`).
- `engine-live`: what each node's mod reports or its Agent SDK or Codex host observes
  (`node_reports`, `node_work`, `sdk_live`, `codex_live`) and what each SDK task measured of its
  conversation (`contexts`). The feeds take the node reports they write as an argument instead
  of reaching them through the engine's live state.
- `engine-worktrees`: each task's worktree (`task_worktrees`), those of cancelled tasks
  (`cancelled_worktrees`) and the startup moves of HOME files (`home_renames`). They carry out
  system commands through the kernel's `DomainRuntime::execute_system_command`.

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

## `core` split into `core`, `domain-rules`, `view-model` and `views` (2026-11-17)

```
alondra, alondrad, scenarios
          │
        views ──────────────┐      the screens: every view and card the clients render
          │                 ▼
          │            view-model  the wire types of a view and the live snapshot the engine writes
          │                 │       ▲
          ▼                 ▼       └── engine-kernel, daemon-engine*, daemon-support: never views
     domain-rules ◄─────────┘      the domain's commands, rules and cards (core::domain before)
          │
        core                       accounts, usage, options, config, the text macros (t!, tf!)
          │
   domain, foundation
```

- The engine's libraries depend on `view-model`, never on `views`.
  Only test binaries that render a screen to check it (the engine's and the kernel's unit tests)
  and the binaries link `views`.
- Each crate has its unit target and compiles only its production sources. The domain tests that
  render screens (they drive the domain and assert on what a client shows) form
  `//crates/views:domain_screens_test`: the domain's unit target cannot link `views`, which links
  the domain. The catalog of tests is the same before and after: 1,665 tests, none added or removed.

Measured with `edit_bench` against main at 84eb0d9, one edit at a time, each edit a static no
earlier run had cached; Bazel's own time without the queue's wait, on a shared machine whose load
changed between runs (the actions are the stable figure):

| Edit | Crates reopened | Test targets reopened | Actions run (rustc / clippy / tests) | Edit + test |
|---|---:|---:|---|---:|
| `core/src/views/task_state.rs` (before) | 23 | 50 | 124 (36 / 16 / 70) | 830 s (a CLI test timed out) |
| `views/src/task_state.rs` (after) | 10 | 37 | 86 (19 / 11 / 55) | 315 s |
| `core/src/views/tests.rs` (before) | 0 | 10 | 19 (1 / 1 / 17) | 65 s |
| `views/src/tests.rs` (after) | 0 | 9 | 16 (1 / 1 / 14) | 21 s |
| `core/src/domain/rules.rs` (before) | 23 | 50 | 123 (36 / 16 / 69) | 370 s |
| `domain-rules/src/rules.rs` (after) | 25 | 55 | 134 (41 / 19 / 72) | 844 s |

- A screen edit compiles 19 crates and test binaries instead of 36, and none of the engine's
  libraries or test-hooks builds.
- A domain edit still reaches everything above it, as expected without early cutoff, and now goes
  through the three crates that replace `core`, one after another, plus their test binaries: 5 more
  compilations. Iterating on the domain alone is cheaper: a line in `rules.rs` with `bazel test` of
  its own target took 41 s (`//crates/domain-rules:domain_rules_test`) against 59 s for
  `//crates/core:alondra_core_test`.

## The cheap levers, measured (2026-11-17)

`edit_bench` now primes rustc's incremental state with a first edit before the edit it times. A crate
taken from the disk or remote cache has no incremental state, so the first edit after a cache hit
recompiles it from scratch: unprimed, the screen edit took 228 s to build with `alondra_lib` alone at
196 s; primed, as in a real edit loop, 59 s. `--build-only` times `bazel build` of the same targets,
`--bazel-flag` passes a configuration to every invocation, and `--tracked` measures the five edits
below. Bazel's own time, without the queue's wait, on the shared Mac while it swapped; each figure is
one run, so the actions and the critical path are the stable part.

| Edit (main at 815dff6) | Build: as is (`-c opt`, clippy) | Build: no clippy | Build: `-c fastbuild` | Build: fastbuild, no clippy | Edit + test: as is | Edit + test: fastbuild, no clippy |
|---|---:|---:|---:|---:|---:|---:|
| `views/src/task_state.rs` | 59 s (21 rustc, 13 clippy) | 35 s | 34 s | 28 s | 261 s | 363 s, `relaunch_test` fails |
| `engine-admission/src/admission.rs` | 39 s (16 / 7) | - | - | 20 s | 321 s | 384 s, a test fails |
| `engine-orchestrator/src/domain_orchestrator.rs` | 19 s (12 / 5) | - | - | 27 s | - | - |
| `engine-live/src/node_reports.rs` | 22 s (22 / 9) | - | - | 26 s | 321 s | 396 s, a test fails |
| `domain-rules/src/rules.rs` | 47 s (62 / 26) | 43 s | - | 53 s | - | - |

- Building is no longer the cost: with incremental state every edit builds in under a minute. Edit +
  test is the time the test targets take to run: the critical path of every edit + test run is a test
  (`engine_sdk_test`, `scenarios_test`, `engine_orchestrator_test`, the daemon's binary tests), and
  the 44 to 58 test targets an engine or screen edit reaches run 550 to 650 s of tests between them.
  They rerun because each links the edited crate.
- `-c fastbuild` builds faster but makes edit + test slower: the libraries the tests link lose their
  optimizations, and `relaunch_test`'s timing fails. Kept at `-c opt`.
- Clippy off the build saves time only when building alone (59 → 35 s for the screen); with tests
  its actions run beside the tests and none is on the critical path. Kept in every build.
- `--@rules_rust//rust/settings:pipelined_compilation=True` fails on this Mac with
  `output file … .rmeta is not writeable` (bazelbuild/rules_rust#3662). Not enabled.
- What reruns is decided by the graph: a test target reruns whenever a crate it links changes, and
  today most test targets link most of the engine. Cutting that is the work of the layering phases.

## Output adapter `github-gh` (2026-11-18)

`edit_bench` on one edit of the `gh` client, before (where it lived, `engine-github/src/domain_github.rs`)
and after (`github-gh/src/cli.rs`), on the same branch and Mac, with incremental state:

| Edit | Crates reopened | Test targets reopened | Actions run, build (rustc / clippy) | Build: critical path | Build: edit + build | Edit + test |
|---|---:|---:|---|---:|---:|---:|
| Before: `engine-github/src/domain_github.rs` | 17 | 51 | 32 (22 / 9) | 37 s | 106 s | 257 s, tests fail under load |
| After: `github-gh/src/cli.rs` | 14 | 42 | 31 (20 / 10) | 30 s | 62 s | 388 s, tests fail under load |

- Building meets the phase's goal (30 s on the critical path) and takes 40 % less in all.
- Edit + test misses it (≤ 90 s): `engine-github`'s test hooks link `github-gh` because their fakes
  (`FakeGitHub`, the scripted `gh` of `domain_github::tests`) build a `GhCli`, and `alondrad`'s tests
  use those fakes. So the edit still reopens 42 test targets, `scenarios_test` (about 180 s) among
  them. Both edit + test runs failed only on tests that pass alone (the SDK host disconnecting at
  start in scenarios, `alondrad`'s PR diff tests), so their times are not comparable.
- Next cut, below: the subsystem's hooks stop linking the adapter.

### `engine-github`'s test hooks without `github-gh` (2026-11-18)

Only two helpers of `engine-github`'s test hooks named a `GhCli`: `github` in `domain_github::tests`
and `answering_gh` in `pr_images::tests`. Both serve only the crate's own unit tests, so they are
`#[cfg(test)]` now and `engine_github_test_hooks` no longer links `github-gh`; the crate's unit test
still does. `crate_layers_test` keeps it that way: test hooks may not link an output adapter that
their crate may not use (`TEST_HOOK_EXCEPTIONS` lists the two that still do, for real hosts).

`edit_bench` on `github-gh/src/cli.rs`, same Mac, with incremental state:

| Hooks | Crates reopened | Test targets reopened | Actions run (rustc / clippy / tests) | Critical path | Edit + test |
|---|---:|---:|---|---:|---:|
| Link `github-gh` (before) | 14 | 42 | — | — | 388 s, tests fail under load |
| Do not (after) | 10 | 37 | 57 (11 / 5 / 40) | 229 s | 239 s, tests fail under load |

- No `daemon-engine`, `engine-admission`, `engine-orchestrator` or `machine-model` test reruns any
  more, and their hooks no longer rebuild.
- Edit + test still misses ≤ 90 s, and this cut cannot reach it: `alondrad` links `github-gh` in
  production, so its tests, `engine_sdk_test` (which runs the `alondrad` binary, about 210 s) and
  `scenarios_test` rerun on every edit and make the critical path. Reaching the goal needs those
  tests to stop depending on the root's link, not on the adapter.
- The run failed only in three scenarios (an SDK host disconnecting at start and timing checkpoints),
  the same failures under load as before; nothing in them touches GitHub.

## Tests leave the root (2026-11-18)

A test reruns when anything it reads changes, a binary in its `data` included, so a test that
depends on the root reruns on every edit of every crate the daemon links. Phase 6 took the two
tests that start the real `alondrad` out of `engine_sdk_test`, into a second target of the same
sources that a name filter and `#[ignore]` selected. They are now a test of the root:

- `//crates/alondrad:codex_adoption_process_test` holds them (a real `alondrad`, killed and
  started again twice, adopts the live Codex host of a task), `manual` and without `#[ignore]`, and
  the release's tests name it. Nothing ran them, and they had stopped passing: the daemon serves
  before it takes over its hosts, and they read the takeover as soon as the listener answered. They
  wait for the owner the store records and the one the host attached to.
- `crate_layers_test` fails on a test outside the root that reaches a crate or a binary of the
  root, through `deps` or `data` at any depth. `ROOT_TEST_CONSUMERS` lists the packages that still
  do: the system and sandbox suites. The server's six tests of the real daemon and CLI live in the
  root as `//crates/alondrad:binary_smoke_test`, so no server test reruns on an edit of the root.
- `test_catalog` lists every Rust test by target and compares two lists, so a change that moves
  tests shows that none was lost.
- `edit_bench` times the build apart from the tests, names the test target that took longest and
  the ones that failed, and `--tracked` measures an adapter, the top of the engine and the root.

`edit_bench` on main at 92cc6f1 with this change, same Mac, with incremental state, Bazel's own time
without the queue's wait:

| Edit | Crates reopened | Test targets reopened | Actions run (rustc / clippy / tests) | Build: critical path | Edit + build | Longest test target | Critical path | Edit + test |
|---|---:|---:|---|---:|---:|---|---:|---:|
| `github-gh/src/cli.rs` | 12 | 39 | 66 (14 / 6 / 45) | 14 s | 21 s | `scenarios_test` 60 s | 81 s | 87 s |
| `alondrad/src/assemble.rs` | 9 | 36 | 55 (8 / 3 / 43) | 11 s | 16 s | `scenarios_test` 53 s | 68 s | 74 s |
| `engine-sdk/src/sdk_owner.rs` | 13 | 43 | 73 (15 / 6 / 51) | 17 s | 31 s | `engine_sdk_test` 222 s | 239 s | 247 s |

- Building is 16 to 31 s of each edit; the rest is tests.
- An edit of an adapter or of the root waits for the longest shard of the conversation scenarios.
- An edit of the engine waits for `engine_sdk_test`, most of whose tests start a real Agent SDK
  host: run one at a time, a test without a host takes 0.05 s (median) and one with a host 0.9 to
  1.8 s.

## A double of `AgentHost` and the contract it shares with the real hosts (2026-11-18)

About 330 tests of `//crates/engine-sdk:engine_sdk_test` start a real Agent SDK host (Node) each:
0.9 to 1.8 s a test, against 0.05 s for a test with no host. The engine reaches hosts only through
the port `AgentHost`, so a double of the port takes the process out of those tests. This step
delivers the double, the contract both implementations pass and the change of the port a double
needs. No test runs on the double yet.

- `engine-doubles/src/agent_host.rs`: `InProcessHosts` implements `AgentHost` with no process and
  no provider's protocol. Each folder holds, in memory, the host that runs there: the bootstrap it
  was launched with, whether it lives, its exit and what was asked of it. A test plays the host
  through the trait `Serve` (what it does on a connection, one end of a `UnixStream::pair()`, and
  when it is asked to stop) and reads what happened (`live`, `launches`, `connections`, `stops`,
  `probed`). A test that restarts its daemon gives the same hosts to the next runtime (`provide`),
  so a host outlives the daemon as a process does. A host's process id is a number above every
  kernel's highest, so it never names a real process, and a provider's command line (`run`) is
  answered by the test and never executed.
- `engine-doubles/src/contract.rs`: `agent_host(hosts, fixture)` is what the engine relies on from
  the port, written against the real adapter first. `engine_doubles_test` runs it on the double and
  `//crates/agent-runtime:owned_host_test` on `ProcessHosts` with its fixture binary
  (`the_process_hosts_keep_the_contract_of_the_port`), so the double cannot drift from the hosts
  the daemon starts. The test of an adapter may link `engine-doubles`, as `system`'s does; the
  crate gained no dependency. The contract launches five hosts one after another in one folder:
  0.54 s on the real adapter, with five processes, and 0.03 s on the double, each run alone on the
  shared Mac.
- What the contract fixed that a double could have got wrong: the caller makes the host's folder,
  private, and a launch in a missing one is refused; a host outlives the handle that launched it;
  the bootstrap of a host that ended is still adopted, and only its connection fails; an adopted
  host is not known, nor stopped, before its endpoint was reached; a claim holds the folder until
  it is retired or dropped, and a bootstrap not retired never lets another host in.
- The handle that stops a host answers for its processes: `HostStop::gone`, `tree` and
  `footprint`. The engine handed a host's process group number to `process-group` in three places,
  which a double could not answer and must never get a real number for: the check of an adopted
  host's process and the processes under a released or halted host (`engine-sdk`), and the memory
  of each node in the machine watch (`engine-admission`). `agent-runtime` makes the same calls
  behind the port now. The machine watch asks `SdkHosts::node_footprints`, answered by the owner of
  each host, in place of `node_groups_by_agent`, which had no other caller and is gone.
- Still around the port, for the steps that move tests onto the double: the Codex provider asks
  `process-group` for its native process, and the engine and the providers read a host folder's
  `host.lock` by its path in a few places, which is why the double writes the bootstrap there too.

## The SDK's tests on the double (phase 5, 2026-11-19)

- `//crates/provider-claude:claude_host_model` is the Claude host the scenarios faked, out of
  `scenarios/src/claude_host.rs`: it speaks the host protocol (hello, challenge, attach, receipts,
  journalled events, acknowledgements) and plays the native Claude behind it. It serves the
  doubled hosts of `engine-doubles` through `Serve`, and the scenarios' relay processes through
  `Process`, so the scenarios use it unchanged. How a turn goes is `Turns`: the test ends it
  (`Played`, the scenarios), Claude answers each prompt at once as the fake native Claude does
  (`Answered`, with `consume-and-hold` kept open and the prompt written to the session's
  transcript), or Claude never takes a prompt up (`Ignored`). It records each native start (its
  session, the one it resumes, the history it had, its folder, command line and environment) and
  every prompt, which is what the tests read of the fake's capture files.
- `engine-sdk/src/model_fixture.rs` (`SdkLaunches`) gives a test's domain the doubled hosts and the
  model, with files nobody runs in place of the executables. `auto_launch` (28 tests), the
  orchestrator's launch tools (110) and the scheduler (29) use it and start no process.
- `engine_sdk_test` and `engine_sdk_host_test` are the same sources; the feature `real-hosts`
  compiles the tests that need the real Node or Codex host or a fake native Claude or Codex
  (`sdk_owner`'s tests, Codex's among them, `account_move`, `bg_handover`, the engine's pause and
  account tests) and leaves the rest out, and without it the engine's test runtime wires the
  doubled hosts in place of `agent-runtime`. The fixtures other crates' tests use
  (`engine_fixtures`: the owner's slice, the automatic launches, the orchestrator's tools) are in no
  library: each test crate compiles them as a module of its own and says which hosts they launch on
  (`crate::engine_tests::SdkLaunches`), the real ones above this crate and the model here.
- `provider-claude/src/host_cases/` holds the behaviour cases, one file per family (turns, the
  native command line): `sdk_worker_test` runs them on the real Node host with the fake native
  Claude, `claude_host_model_test` on the model, with the same expectations. A model that answered
  a prompt without starting it failed the turns case while the real host passed it.
  `agent-host:sdk_worker_test` checks the worker's `--model`, `--effort`, `--disallowedTools`,
  `--session-id` and `--resume` itself.

Measured on the shared Mac under load, one run each:

| | Before | After |
|---|---:|---:|
| `engine_sdk_test` | 222 s, about 550 tests | 18.8 s, 260 tests |
| `engine_sdk_host_test` | (none) | 144.5 s alone, 164 s in the edit below |
| Median per test run serially: `auto_launch` / launch tools / scheduler | 0.9 to 1.8 s with a host | 0.079 / 0.062 / 0.037 s |
| `edit_bench` on `engine-sdk/src/sdk_owner.rs`: edit + test | 247 s (main at 92cc6f1, above) | 187 s, critical path 171 s |

- The edit + test still waits for `engine_sdk_host_test`, now the longest target; its goal
  (≤ 110 s) is not met. What is left in it is `sdk_owner`'s own tests (144, Codex's 70 among
  them), which need the real hosts or behaviours of the fake native Claude the model does not
  play yet (decisions, background commands, refusals, resumption without history).
- No check fails when a host executable goes back into `engine_sdk_test`'s `data`:
  `crate_layers_test` only follows the root.

## No port written against the concrete runtime (phase 4d, 2026-11-21)

`DomainRuntime::of`, the bridge that turned the `&dyn Runtime` a port is called on back into the
kernel's own type, is gone. Every implementation of a port in `engine-admission`, `engine-sdk` and
`daemon-engine` works on any `Runtime`, and so do the pieces they call (`engine-live`,
`engine-github`, `engine-worktrees`, `engine-orchestrator`'s view of the engine, the kernel's commit
observers and settings projection).

- `Runtime` (`engine-ports/src/runtime.rs`) gained the concrete methods the pieces used on the
  kernel, rust-analyzer style: the live read and the read index, the journal with its launch queue,
  the subscriptions to revisions, snapshots and schedules, commands as Alondra or as an authorized
  actor, owner transactions (with or without a projection outside the journal), the seals of a
  task's observations, the held messages, the capabilities (bind, revoke, resolve a bearer, admit a
  change, a session's digest), a transport's receipt and a task's public conversation. The cards
  Alondra raises and withdraws are default methods built on its system commands.
- What only reads or writes the journal is `RuntimeRecords` in `engine-kernel`, implemented for every
  `Runtime` (a task's worktree, the inherited worktrees, spend and PR sizes, model decisions, session
  starts, request launches, the stored launch plan with its storage error, attachments).
- `Principal` and `Capability` moved to `foundation/src/principal.rs`, so the trait can name them;
  `daemon-support`'s authority re-exports them. The hooks' worker pools are `daemon-engine`
  extensions (`TransportWork`) instead of kernel fields.
- `the_ports_run_on_a_runtime_that_is_not_the_kernels` (`alondrad`) calls a port of each piece on
  `engine_doubles::runtime::Forwarded`, a runtime that hands every call to the kernel's: with the
  bridge back in one port it panics on the downcast, without it it passes.
- `engine-admission` no longer depends on `engine-kernel` outside its tests: `LAYER_EXCEPTIONS` drops
  `engine-admission → engine-kernel` (54 → 53 entries).

## `engine-sdk` asks the engine through `SdkEngine` (phase 4c-1, 2026-11-21)

`engine-sdk`'s code no longer calls `daemon-engine` or `engine-admission`: what it asks of the engine
goes through the port `SdkEngine` (`engine-ports/src/sdk.rs`), which `daemon-engine` implements
(`wiring::Engine`, like `AdmissionEngine`), in four groups: accounts for a launch, launching and
relaunching, the `claude --bg` nodes, and the hosts' reports. `domain.engine()` is that port for
one runtime. `the_daemon_implements_every_port` requires it.

- What was pure moved down: the admission gate's wrapper (`GateOf`/`Gate`) and the node-close types
  to `engine-ports`; `untaken`, `start_failures`, the accounts file, the launches and the background
  sessions to `engine-kernel`; `claude_trust` to `daemon-engine-model`. `daemon-engine` re-exports
  them, so its other callers did not change.
- The engine is put together in `alondrad` (`open`, `open_at` and the ordered commit observers);
  `daemon-server` receives the opener from the root (`preflight::run`, `DomainDaemon::open`). The
  engine's tests open it with a copy of the observers (`engine-sdk/src/engine_assembly.rs`), and
  `the_engines_tests_hear_of_commits_as_the_daemon_does` fails when the two lists differ or change order.
- The fixtures that open the whole engine left the `engine-sdk` library: `engine_fixtures.rs` and
  the files it names are sources of the test targets that use them (`engine_sdk_test`,
  `engine_sdk_host_test`, `daemon_server_test`, `codex_adoption_process_test`), which compile them
  by path, as they do `owner_fixtures.rs`. The library no longer depends on `daemon-engine` or
  `engine-admission`, their two entries left `LAYER_EXCEPTIONS`, and an edit of either crate no
  longer rebuilds `engine-sdk` or what links it.
- `engine-sdk`'s own tests still linked the whole engine, so they reran on an edit of
  `daemon-engine` or `engine-admission`; "`engine-sdk`'s tests without the engine" takes the ones
  that do not need it out.

Measured with `edit_bench`, one run each on the shared Mac under load (load average about 14).

| Edit | Crates reopened | Test targets reopened | Actions run (rustc / clippy / tests) | Edit + build | Longest test target | Edit + test |
|---|---:|---:|---|---:|---|---:|
| `engine-sdk/src/sdk_owner.rs` (before, main at 97f5f5f) | 10 | 43 | 86 (12 / 6 / 67) | 26 s | `engine_sdk_test` 189 s | 206 s |
| `engine-sdk/src/sdk_owner.rs` (after) | 10 | 43 | 84 (12 / 6 / 65) | 38 s | `engine_sdk_test` 290 s | 349 s |

- No build-time win, as expected: the edit reaches the same crates and test targets, because the
  library's fixtures keep the engine among `engine-sdk`'s dependencies. The slower "after" figures
  come from the load; the same targets took 263 s (`engine_sdk_test`) when run again. A test failed
  in each run (`scenarios_test` before; `relaunch_test` and `scenarios_test` after); each passed when
  run again alone.

## `engine-sdk`'s tests without the engine (2026-11-22)

`engine-sdk`'s tests are three targets of the same sources, split by two features:

| Target | Features | Links `daemon-engine` and `engine-admission` | Tests |
|---|---|---|---:|
| `engine_sdk_test` | none | no | 54 |
| `engine_sdk_engine_test` | `whole-engine` | yes | 224 |
| `engine_sdk_host_test` | `real-hosts`, `whole-engine` | yes | 311 (3 ignored) |

- `engine_sdk_test` runs what needs no engine: the pure tests (`idle_release`, `ended_hosts`,
  `host_folders`, `claude_history`, `conversation_file`, `node_environment`, `session_refresh`) and
  the scheduler's tests that open a runtime (`domain_schedule_runner`, `domain_schedule_api`). Their runtime is
  `model_fixture::open`: the crate's own wiring and commit observers, the doubles of
  `engine-doubles` for the Mac, the agent hosts, the gate, admission and the orchestrator, and
  `engine-doubles/src/sdk.rs` for `SdkEngine`.
- That double of `SdkEngine` answers only what a launch and the hosts' owners ask (the account and
  model, whether accounts are still being checked or wait for a session, whether a worktree is
  ready, the last request the daemon timed) from a script the test writes, and takes what the hosts
  report about a node without doing anything. Every other question panics with its name and says
  the test belongs in `engine_sdk_engine_test`; it also writes that line straight to stderr, because
  the owners ask from threads whose panic the test harness keeps until the test ends.
- The three scheduler tests that read the engine's own usage pause, disk watch or admission gate
  are `domain_schedule_runner/engine_tests.rs`, with the feature `whole-engine`; the scheduler's
  fixture (`domain_schedule_runner/fixture.rs`) opens the runtime of either target.
- `engine_sdk_engine_test` is the old `engine_sdk_test` minus those: the engine's own tests that
  live here (`engine_tests/*`), `auto_launch` and `task_closer` end to end, on the doubled hosts.
  `engine_fixtures`, `engine_assembly` and the test `use`s of `daemon-engine` in `lib.rs` compile
  only with `whole-engine`. `engine_sdk_test` runs no program; the git the close of a task asks
  about went with `task_closer` to `engine_sdk_engine_test`'s `TEST_EXECS`.
- `tools/layers/crate_layers_test.py` fails when `engine_sdk_test` reaches `daemon-engine` or
  `engine-admission` again, through anything it reads at any depth (`TESTS_KEPT_APART`). Checked by
  hand: putting `engine-admission` back in its `deps` fails the check, taking it out passes.
- The tools that tell test-only code from production (`prose_decisions`, `module_graph`) take any
  `#[cfg(all(test, ...))]` as test-only, not only the `real-hosts` ones.

Counts, `engine-sdk`'s targets, before (main at 765f540af) and after: 274 + 308 = 582 tests
before, 50 + 224 + 308 = 582 after. On main at 0b09cefb5, which adds `host_folders` and its 4
tests, they are 54 + 224 + 311: those 4 need no engine and run only in `engine_sdk_test`, where
under `#[cfg(test)]` they ran in `engine_sdk_host_test` too.

Measured with `edit_bench`, one run each on the shared Mac under load (load average 20 to 45):

| Edit | Test targets reopened | Actions run (rustc / clippy / tests) | Edit + build | Longest test target | Edit + test |
|---|---:|---|---:|---|---:|
| `daemon-engine/src/wiring.rs` (before) | 46 | 117 (14 / 8 / 94) | 24 s | `engine_sdk_host_test` 217 s | 247 s |
| `daemon-engine/src/wiring.rs` (after) | 46 | 110 (14 / 8 / 87) | 26 s | `engine_sdk_host_test` 214 s | 237 s |
| `engine-sdk/src/sdk_owner/session_refresh.rs` (before) | 45 | 99 (13 / 7 / 78) | 22 s | `engine_sdk_host_test` 184 s | 205 s |
| `engine-sdk/src/sdk_owner/session_refresh.rs` (after) | 46 | 101 (14 / 8 / 78) | 23 s | `engine_sdk_host_test` 262 s | 286 s |

| Target | Before | After |
|---|---:|---|
| `engine_sdk_test`, its tests' own time | 32.0 s, 274 tests | 0.6 to 1.1 s, 50 tests |
| `engine_sdk_engine_test`, its tests' own time | (none) | 21.8 to 27.3 s, 224 tests |

- An edit of `daemon-engine` or `engine-admission` no longer reruns `engine_sdk_test`, and an edit
  of the scheduler or of the pure pieces has a target of about a second that waits for neither
  crate to compile. The edit + test of either edit is not shorter: it still waits for
  `engine_sdk_host_test`, whose time moves with the load more than with this change (184 to 262 s
  for the same tests). An edit of `engine-sdk` now reopens one more target, the third.
- The before and after runs each had a failure the change does not explain: the release test
  `a_large_conversation_whose_cache_expired_goes_on_in_a_new_session_that_can_read_the_old_one` of
  `engine_sdk_host_test` timed out its 60 real seconds on main and on this branch under load, and
  passed when the load fell. `engine_sdk_engine_test` failed twice in about ten runs under load; the
  run whose log was kept shows the scheduler's fixture removing its folder while its runtime was
  still alive and writing there ("Directory not empty"), code it kept from main. Its runtime is now dropped before the folder, which
  `test_support::Directory` removes.
- **Not done:** the tests that use the engine only as a collaborator (`sdk_owner`'s, Codex's,
  `release_tests`, about 250) are not ported to doubles: they need the real hosts, and a double of
  all 43 methods of `SdkEngine` would only move the engine into the doubles. The engine's own tests
  that live here (`engine_tests/*`) did not move to `daemon-engine`'s crate either.

## The owner's tests on the model (phase 6, part 1, 2026-11-22)

`sdk_owner`'s tests (`tests.rs`, `release_tests.rs`, `untaken_tests.rs`, `capability_tests.rs`,
`pause_tests.rs`) compiled only in `engine_sdk_host_test`. They now compile in
`engine_sdk_engine_test` too, and each test says where it runs:

- The owner's slice (`engine_fixtures::owner::Slice`) takes its hosts from the crate that compiles
  it, `crate::engine_tests::SliceHosts`: `ProcessHosts` (the pinned Node host and the fake native
  Claude, read back from its capture files) in `engine_sdk_host_test`, `daemon_server_host_test` and
  `codex_adoption_process_test`; `engine_fixtures::model_hosts::ModelHosts` (the doubled hosts of
  `engine-doubles` playing the Claude host model) in `engine_sdk_engine_test` and
  `daemon_server_test`, which compiles the launches it builds on (`model_fixture/launch_fixture.rs`) by
  path as `crate::model_fixture`. What a test reads of the hosts (the
  starts, the prompts, the control requests and the decisions) goes through the trait `Hosts`, and
  so does killing a test's host (`Slice::kill_host`).
- A test that passes on the model is `#[cfg(not(feature = "real-hosts"))]` and runs only in
  `engine_sdk_engine_test`; one that needs the real host, kills its process group or follows a
  scripted scenario of the fake native Claude is `#[cfg(feature = "real-hosts")]` and stays in
  `engine_sdk_host_test`. Each was sorted by running it alone on the model with a 60 s limit:
  62 pass (`tests.rs` 40, `release_tests` 8, `untaken` 5, `capability_tests` 3, `pause_tests` 6),
  the rest fail or hang. A helper takes the mark of the tests that use it, none when both do.
  `codex_tests` stays whole in `engine_sdk_host_test` (the Codex host, phase 8).
- What keeps the rest on real hosts, counted over the failures of the tests that compiled on the
  model: a scenario the model does not play (the turn rests where the fake would ask, plan or hold:
  20), `kill_host`'s `/bin/kill` on the host's group (20), the control requests (6) and decisions (4)
  the model does not keep, a hang past 60 s (5), and one or two each of a process group, a test
  process holding a host and files only the fake writes. Another 74 tests and helpers do not
  compile on the model because they read the fake's capture files or the host root directly.

Counts and times, `bazel test` on the shared Mac:

| Target | Before (main at a4ff450e2) | After |
|---|---:|---:|
| `engine_sdk_engine_test` | 231 tests | 293 tests, 11.1 s |
| `engine_sdk_host_test` | 310 tests (144.5 s alone, 164 s in an edit, measured before) | 248 tests, 125.3 s |

`//tools:test_catalog compare` over `//crates/engine-sdk/...`, `//crates/daemon-server/...` and
`codex_adoption_process_test`: 1305 kept, 62 now also listed in `engine_sdk_engine_test`, none lost.

## Decisions, controls and dead hosts on the model (phase 6, part 2, 2026-11-22)

The Claude host model (`crates/provider-claude/model/fake_claude_host.rs`) now plays what kept 39
more of the owner's tests on real hosts:

- **Decisions.** A prompt `decide:<scenario> <argument>`, also relayed as `[From <who>] decide:…`,
  makes Claude call a tool its rules do not allow (`bash`, `bash@<folder>`, `question`,
  `questions`, `plan`, `unwritten-plan`, `alondra`, `imitation`) or an MCP server ask for input
  (`elicitation`), as the fake native Claude does. The host asks the daemon with the payload the
  real host sends (`{tool, input, request, cwd}`, or the server's form) and the turn waits. A
  `respond` reaches only the request pending under exactly its identity (epoch, kind, tool use,
  payload), else its receipt is `stale_request`; the SDK's response to Claude (the host's
  `permissionResult`, `planResult` and `elicitationResult`, with the SDK's `toolUseID`) is kept as
  the fake keeps it, and the turn ends. An interrupt withdraws what Claude waits on: each request
  settles `cancelled` and Claude gets `Permission request cancelled`.
- **Controls.** `interrupt`, `set_model` (refused with `unsupported_model` for a model Claude does
  not offer), `set_permission_mode` and `mcp_status` (with its `control_result`) are kept as the
  SDK sends them.
- **Dead hosts.** `Hosts::kill` kills the host's process group on real hosts and ends the doubled
  host on the model (`ClaudeModel::kill_process`), with no `/bin/kill`.

The shared behaviour cases gain a family, `host_cases/decisions_tests.rs`, run by `sdk_worker_test`
against the real Node host with the fake native Claude and by `claude_host_model_test` against the
model: the payload of a permission, a question, a plan, Alondra's tool and a server's form must be
the same on both. Removing `requestId` from the model's request fails the permission case on the
model and not on the real host.

39 tests moved (decisions 24, controls 6, killed hosts 9); 6 that compiled on the model still
fail there and stay: they need a process group (`cancelling_an_sdk_task_mid_turn…`), the
`…unread` scenario, a plan Claude asks in a turn of its own, or what an ended host's journal held.
Four that kill a host also hold its prompts with `HOLDING_NATIVE`, a script only real hosts run.

| Target | Before (main at 6e73fadf5) | After |
|---|---:|---:|
| `engine_sdk_engine_test` | 293 tests | 332 tests, 11.0 s |
| `engine_sdk_host_test` | 248 tests, 123 s | 209 tests, 82.0 s |

`//tools:test_catalog compare` over `//crates/engine-sdk/...`, `//crates/daemon-server/...`,
`codex_adoption_process_test` and `//crates/provider-claude/...`: 1650 kept, 49 new (the 39 that
moved, now listed in `engine_sdk_engine_test`, and the 5 decision cases in each of the two case
targets), none lost.


## Admission and orchestrator without the engine (phase 4a, 2026-11-18)

`engine-admission` and `engine-orchestrator` compile and test without `daemon-engine` and without
`test-hooks` builds of their own: their doubles are registered at run time from the test-only crate
`engine-doubles`, and the `engine-admission → daemon-engine` exception left `LAYER_EXCEPTIONS`
(55 → 54 on main at 3a9431c). `engine-github` keeps its `test-hooks` build; it goes with phase 4b. An edit to `daemon-engine`, `engine-sdk`, `engine-github`,
`github-gh`, `engine-live`, `engine-mcp` or `engine-worktrees` no longer reruns the gate's or the orchestrator's
tests, and their MUSTS checks no longer list those crates.

Measured with `edit_bench`, one edit at a time; Bazel's own time without the queue's wait, on the
shared Mac. A test failed in each run before and after (the bench does not name it); the same targets
passed when run again alone.

| Edit | Crates reopened | Test targets reopened | Actions run (rustc / clippy / tests) | Critical path | Edit + test |
|---|---:|---:|---|---:|---:|
| `engine-admission/src/admission.rs` (before, main at 5a71f13) | 13 | 44 | 75 (16 / 7 / 51) | 288 s | 329 s |
| `engine-admission/src/admission.rs` (after) | 13 | 43 | 79 (15 / 6 / 57) | 216 s | 243 s |
| `engine-orchestrator/src/domain_orchestrator.rs` (before) | 11 | 41 | 63 (12 / 5 / 45) | 275 s | 293 s |
| `engine-orchestrator/src/domain_orchestrator.rs` (after) | 11 | 41 | 63 (12 / 5 / 45) | 245 s | 274 s |

- The target of the phase (≤ 30 s build, ≤ 90 s edit + test) is not met. An edit to these crates
  still reaches every crate above them (`engine-sdk`, `alondrad`, the scenarios, the machine model),
  and their test targets dominate the time. What this phase cuts is the other direction: edits below
  the gate and the orchestrator no longer reach their tests.
- The rest is phase 4b: the remaining `test-hooks` builds (`engine-github`'s among them) and the `*_on_test_hooks` copies go, so no
  crate is compiled twice.

## No crate compiled twice (phase 4b, 2026-11-19)

The `test-hooks` feature and its 14 second builds are gone: the twelve `*_test_hooks` libraries and
the two `*_on_test_hooks` copies (`engine_admission`, `engine_orchestrator`). Each crate compiles
once, and its tests link that same library.

- What only tests change is decided at run time by `alondra_foundation::under_test()`, which reads
  `BAZEL_TEST` once: an empty Claude folder of the test's own, no flush after atomic writes, no
  wait before relaunching a host, shorter acknowledgement and settings waits, and the check of each
  owner transaction against a reload. The installed daemon carries these branches, inert without
  that variable ([on your Mac](../on-your-mac.md)).
- Fixtures other crates' tests use are production modules (`testing` and `retention_fixtures` of
  `store-sqlite`, the kernel's `runtime_owner`, the `*_fixtures` modules of `daemon-engine` and
  `engine-sdk`); what one crate alone uses stays in `#[cfg(test)]`. The owner's fixtures, which
  read the SQLite journal, are compiled by no production crate: `daemon-engine`, `engine-sdk` and
  `daemon-server` include `wiring/owner_fixtures.rs` in their test binaries (`#[path]`), so the
  engine stays off the `store-sqlite` adapter.
- The gate, the machine doubles, the journal and the runtime of a test are registered at run time
  by the test that opens the runtime (`wiring::open_with`, `TEST_JOURNAL`,
  `engine_admission::for_tests`).
- `crate_layers_test` fails on a `testonly` library that builds a production crate again, and
  `TEST_HOOK_EXCEPTIONS` (agent-runtime and store-sqlite for `daemon-engine` and `engine-sdk`)
  left the table.
- The scenarios now run the engine at full optimization instead of the low-optimization copies,
  which exposed a race in their harness: between two turns it waited only a moment of calm, not
  the 250 ms a host's owner may take to look at a new input, so the virtual clock sometimes moved
  on first. It now waits that long whenever the domain holds an input a running host was not
  offered. `bazel run //crates/scenarios:accept` sets `BAZEL_TEST`, so a case is accepted as the
  test plays it.

Measured with `edit_bench`, one edit, before (on main with phase 5b, before any change) and after; Bazel's own
time without the queue's wait, on the shared Mac:

| Edit | Crates reopened | Test targets reopened | Actions run (rustc / clippy / tests) | Build: critical path | Edit + test |
|---|---:|---:|---|---:|---:|
| `engine-live/src/node_reports.rs` (before) | 17 | 50 | 87 (21 / 8 / 57) | 19 s | 287 s (a test failed under load) |
| `engine-live/src/node_reports.rs` (after) | 12 | 49 | 83 (16 / 8 / 58) | 29 s | 254 s |

- The edit compiles 16 crates and test binaries instead of 21. Its build time rose with the
  Mac's load between runs; the action count is the stable figure.
- Edit + test is still dominated by `engine_sdk_test` (211 s of the 236 s critical path), whose
  tests start real Agent SDK hosts one at a time. The phase target (≤ 90 s edit + test) is not met
  for edits that reach `engine-sdk`.

## Screens and input adapters (phase 6, 2026-11-18)

```
roots:        alondrad (main, supervision, assemble + wire)    alondra (main, wire, dispatch, log catalog)
                 │  registers views as `Screens`                  │  installs the transport and the hooks
adapters-in:  daemon-server                     cli · cli-tui · cli-mcp · cli-report · client-transport
                 │  routes, iroh, direct,          │  each one only through ports
                 │  pushes, the screens' cache     │
ports:        engine-ports (+ `Screens` in its registry) · view-model (`Screens`, the screens' types)
              client-port (the daemon client's traits, its protocol `Client`, shared CLI words)
application:  views (implements `Screens`; only the roots link it) · the engine
```

- `view-model` holds the port `Screens` (`build`, `inbox_badge`, `activity_counts`,
  `waiting_cards`, `push_content`, `answer_label`) and everything a client or the server names
  besides the rendering itself: `RenderContext`, `BuildError`, the protocol constants, the push
  types, capabilities, outcomes and refusals, the accounts' and metrics' words, the keys of each
  screen. `views` re-exports them, so its own code did not change, and implements the port
  (`alondra_views::Views`, `alondra_views::wire`).
- `daemon-server` is everything `alondrad` held but `main`, the supervision and `assemble`. It
  asks the screens of the registry (`screens::install`, `screens::get`); its own tests render with
  the real `views`, the only test target of the server that links it. Until phase 4 puts the
  engine behind ports it depends on five engine crates and `github-gh` through `LAYER_EXCEPTIONS`
  (54 → 60), the
  user's choice.
- The CLI is five adapters and a port. `client-port` holds the transport traits and the protocol
  `Client` built on them; `client-transport` implements them (HTTP to this Mac, the relay, iroh).
  The root `alondra` installs the transport, the account keychain, the doctor's diagnosis and the
  log catalog, and hands each command to its adapter. No adapter depends on another one, and no
  layer exception was added.
- The log catalog is built for the root alone: a word added in any crate rebuilds the root, not a
  library of the CLI.
- The engine's tests no longer link `views`. The ones that checked a screen after driving the
  engine run with the server's tests (`daemon-server/src/engine_tests/`), whose fixtures the engine
  crates expose under `test-hooks`; the admission menu's wording was already a test of `views`.
  `engine_sdk_test` (150 to 227 s) carried the `alondra` and `alondrad` binaries for two opt-in
  tests: they moved out of it, so relinking the binaries no longer reruns the SDK suite (they are
  a test of the root now, see "Tests leave the root").
- `scenarios_test` reruns on every edit of any crate, and it ran its 16 cases one after another
  in 124 s. Its runner already split cases by Bazel's shard variables: it now runs in 4 shards, the
  longest 41 s.

Measured with `edit_bench --edit crates/views/src/task_state.rs` on the shared Mac, the incremental
state primed; its load average was 45 to 51 during the last runs:

| `views/src/task_state.rs` | Crates reopened | Test targets reopened | Actions run (rustc / clippy / tests) | Critical path | Build | Edit + test |
|---|---:|---:|---|---:|---:|---:|
| Before (plan, unprimed) | | | 21 / 13 | 220 s | 228 s | 315 s |
| Before (phase 1, primed) | | | 21 / 13 | | 59 s | 261 s |
| Phase 6, `scenarios_test` in one shard | 10 | 43 | 77 (17 / 11 / 48) | 168 s | 32 s | 184 s |
| Phase 6, `scenarios_test` in 4 shards | 10 | 43 | 84 (17 / 11 / 55) | 74 s | 30 s | 105 s |

- The libraries a screen edit rebuilds are `views` and the roots (`alondrad`, `alondra`,
  `scenarios`); no adapter and no piece of the engine.
- What is left of edit + test is the tests that link `views` or run the binaries: the four
  scenario shards (up to 41 s), `daemon_server_test` (26 s alone, 55 s on the loaded Mac), the
  binaries' tests (`relaunch_test`, `doctor_service_test`, `cli_api_test`, 18 to 30 s) and the
  views' own. rules_rust 0.74 does not shard `rust_test`.

- Next cut: move the fakes that need a `GhCli` into `github-gh`'s own test hooks, so the subsystem's
  hooks stop linking the adapter.

## Where a scenario's real seconds go (phase 2, 2026-11-19)

The runner now prints each case's real time and its breakdown. 93 to 98% of a case was the runner's
own wait for calm (`settle`: 2 ms sleeps, 25 quiet looks in a row, 150 while an input waits on the
owner's wake), once per virtual second and after every step. The other suspects were small: the
`F_FULLFSYNC` calls of one 12 s case took 0.31 s, its 26 process spawns ran beside the wait, and
assembly took 0.05 to 0.12 s. Shortening the sleep moved a checkpoint by 2 s, so the calm criterion
stays. What changed: a virtual clock keeps the deadlines of the timed waits pending on it (every
sleep, interval, timeout and monitor wait registers its deadline when it is made and forgets it when
it ends or is dropped), and before each virtual second the runner asks whether one falls inside it
(`clock::deadline_within`). When none does, the advance can wake nobody, on whatever thread, and
the runner does not wait for calm after it: 439 of 1,347 virtual seconds over the 16 cases. The
others still wait, including the about 900 that wake a loop which changes nothing; those belong to
the harness.

Each case in its own shard of `scenarios_test` (16 shards, `--nocache_test_results`), before this
change in one run and after it in three; every accepted checkpoint is unchanged:

| Case | Before | After |
|---|---:|---:|
| answering-one-pr-card-keeps-the-other | 4.46 s | 3.04 to 3.39 s |
| codex-turn-cut-and-resumed | 0.99 s | 0.74 to 0.98 s |
| draft-pr-delivery-refused-until-ready | 6.49 s | 3.88 to 4.52 s |
| integrated-delivery-presented-again-is-a-report | 13.11 s | 8.53 to 9.63 s |
| login-expired-holds-only-its-account | 13.24 s | 8.40 to 9.66 s |
| one-task-refused-while-others-work-asks-after-the-probe | 5.16 s | 4.16 to 4.75 s |
| paused-node-coming-back-can-be-paused-again | 12.49 s | 9.45 to 11.03 s |
| provider-outage-opens-no-refusal-cards | 8.04 s | 5.29 to 5.99 s |
| reply-after-login-expired-on-integrated-delivery | 5.00 s | 3.25 to 3.44 s |
| restart-loses-a-host-and-a-retake-brings-it-back | 10.20 s | 5.92 to 6.96 s |
| restart-mid-turn-keeps-the-work | 12.98 s | 8.11 to 9.35 s |
| stack-declared-retargeted-before-merge | 4.45 s | 2.87 to 3.28 s |
| stack-undeclared-retargeted-before-merge | 7.91 s | 5.09 to 5.99 s |
| update-replaces-hosts-it-cannot-adopt | 7.21 s | 4.46 to 4.71 s |
| usage-limit-closes-merged-delivery | 7.67 s | 5.10 to 5.71 s |
| user-message-survives-a-restart | 11.94 s | 7.33 to 8.63 s |

The 16 cases sum 131.3 s before and 85.6 to 98.0 s after. Since the wait is idle,
the cases now run in 16 shards. `scenarios_test` with `--nocache_test_results` (the first three
rows before this change, with three runs each unless noted):

| Shards | Test target (longest shard) | Peak memory, each shard / all shards |
|---|---|---|
| 4 | 39.6 s (one run) | 59 MB (one shard) / about 0.24 GB, estimated |
| 8 | 23.0, 21.4, 21.5 s (a fourth run: 21.5 s, one case failed on the known usage-gauge timing) | 53 to 57 MB / 0.44 GB |
| 16 | 18.2, 17.7, 16.9 s | 49 to 58 MB / 0.85 to 0.87 GB summed, at most 11 at once |
| 16, after this change | 15.0, 14.7, 13.3 s | 50 to 58 MB / 0.87 to 0.88 GB summed |

These rows predate the change that holds virtual time for the daemon threads the clock wakes.
With it, on 20 cases, `paused-node-coming-back-can-be-paused-again` takes 51.5 s (three runs:
51.46, 51.50, 51.60 s, 43 s of them in its rests) with or without this change, so the longest
shard is that case; the other 19 take 4 to 15 s.

A shard spends 1.4 to 4.9 s outside its case (start and end of the process and of the test
wrapper); where is not measured.

The durability of `engine_sdk_test`, sampled with `/usr/bin/sample` every 10 ms over one full run
(545 tests, 141 s): SQLite commits plus `sync_all` are 28% of the summed time of the test threads
(428 of 1,529 thread-seconds). The sync calls a durability switch would remove, SQLite's `fsync`
and `sync_all`'s `F_FULLFSYNC`, are 5.5% (84 thread-seconds); the weight is the WAL `pwrite`
inside the commits (339 thread-seconds). The share of wall-clock time, the contention between the
11 or so tests that run at once (the Mac was also swapping), and the sampler's own cost are not
measured.

That switch is `alondra_foundation::durability` (2026-11-22). The engine's test fixtures call
`relax()` when they wire a runtime (`wired()` in `engine-sdk`, `daemon-server` and `alondrad`); from
then on the process opens the domain database with `synchronous=OFF` and `durability::flush` skips
`sync_all` in the SQLite store, the capability registry, the session settings, the managed
documents, the orchestrator's owner file, the retired host folders and the home renames. `relax()`
panics outside `bazel test`, so the daemon always opens with `synchronous=FULL`, and the scenarios
never relax it. One run of `engine_sdk_test` each, on a shared Mac: 9.6 s before, 7.0 s after; one
run does not separate the switch from the machine's load, and the sampled 5.5% is the share it can
remove.

## Output adapter `system`: git, worktrees and disks (2026-11-18)

`crates/system` runs git, makes and deletes the task worktrees and reads the disks. The engine asks
it through the `Worktrees` and `Disk` ports of `engine-ports`, and only the `alondrad` root links it.
The clock, the daemon's log, its locks and its atomic saves moved to `foundation`, so the adapter needs
nothing of the application. The engine's tests run against `engine-doubles`' git-free `Worktrees` and
`Disk`, which pass the same contract suite (`engine-doubles/src/contract.rs`) that `system`'s tests
run with real git. `edit_bench` on one edit of the worktree code, before (where it lived,
`daemon-support/src/worktree.rs`, main at 3a9431c) and after (`system/src/worktree.rs`), on the same
Mac with incremental state:

| Edit | Crates reopened | Test targets reopened | Actions run, build (rustc / clippy) | Build: critical path | Build: edit + build | Edit + test |
|---|---:|---:|---|---:|---:|---:|
| Before: `daemon-support/src/worktree.rs` | 35 | 76 | 66 (48 / 17) | 32 s | 48 s | 312 s, a test fails under load |
| After: `system/src/worktree.rs` | 11 | 38 | 15 (10 / 4) | 10 s | 22 s | 87 s |

- The edit rebuilds `system`, its test, `alondrad` and the tests that run the `alondrad` binary.
- Before the doubles, with the engine's test hooks wiring `system` into every runtime, the same edit
  took 46 s to build and 318 s with tests.

## Output adapter `system`: the Mac's gauges (2026-11-18)

The memory, load, swap and own-disk readings the admission gate decides on (sysctl, Mach's
`host_statistics64`, `sysinfo`) moved from `daemon-engine-machine` to `system/src/machine.rs`, behind
the `MachineProbe` port of `engine-ports`. `engine-admission` keeps every decision and asks the port
for the Mac's size when it has no sample yet; the engine's tests get `engine-doubles`' calm Mac, and
both pass `contract::machine`. No application crate links `sysinfo` any more. `edit_bench` on one edit
of the reading code, where it lived (`daemon-engine-machine/src/machine.rs`, which still holds the
sampling loop) and where it lives now, on the same Mac with incremental state and a load near 19:

| Edit | Crates reopened | Test targets reopened | Actions run, build (rustc / clippy) | Build: edit + build | Edit + test |
|---|---:|---:|---|---:|---:|
| Where it lived: `daemon-engine-machine/src/machine.rs` | 31 | 69 | 58 (42 / 15) | 30 s | 258 s, a test fails under load |
| Now: `system/src/machine.rs` | 11 | 38 | 15 (10 / 4) | 20 s | 82 s |

## Output adapter `system`: accounts, processes, plugins and sign-in returns; foundation `host` (2026-11-18)

The engine reaches a provider account's folders, the Codex app-server, Claude Code's trust, the
processes it signals, Alondra's installed plugins and Claude's MCP sign-in listeners through the
`Accounts`, `Processes`, `Plugins` and `SignInReturns` ports; `system` implements them and only the
roots link it. Claude's sign-out, Codex's and the MCP probe run through the `AgentHost` port that
already ran Claude's status. The engine's tests run on the doubles of `engine-doubles`, which pass
the same contract suite as `system`; a `system` test checks that the Codex double answers what the
repository's fake Codex answers to a real probe.

What the CLI, the server and the clients reach by themselves (Bonjour, the Claude Code install and
its updater, the launcher, the record of failed starts, the Homebrew install, the user the process
runs as) cannot go to `system`, which an input adapter may not link: it moved from `core` to
`host`, a foundation crate only its users link. The types and pure parsing stay in `core`.

`edit_bench` on the same Mac, with incremental state, under the shared load of the day:

| Edit | Crates reopened | Test targets reopened | Edit + build | Edit + test |
|---|---:|---:|---:|---:|
| Before: `core/src/codex_account.rs` (the account code in `core`) | 57 | 132 | - | - |
| After: `system/src/codex_account.rs` | 11 | 38 | 14 s | 91 s |
| After: `host/src/claude_update.rs` | 41 | 89 | - | 316 s |

- An edit of the account code now rebuilds `system`, its test, `alondrad` and the tests that run the
  `alondrad` binary, as an edit of the worktree code does.
- `host` is still linked by `daemon-support`, `daemon-engine` and `engine-kernel` (the user name,
  the programs on the PATH, the Claude Code release check, the record of failed starts), so an edit
  of it reopens most of the engine; less than when the same code was in `core`.

## The store behind `Journal` (phase 5e, 2026-11-19)

- `store-sqlite` is the old `daemon-engine-store` with `domain_database_migration` (from
  `engine-kernel`) and `database_check` (from `daemon-engine`). What only decides moved to
  `domain-rules`: `acceptance_cases`, the wording the orchestrator is told, and which stored card is
  one of Alondra's own (`SystemCard::of_stored`); the store keeps the SQL that upgrades old
  databases.
- `engine-kernel`, `daemon-engine`, `daemon-engine-machine` and `engine-sdk` no longer depend on the store
  in production: four layer exceptions go. The fixtures `daemon-engine` and `engine-sdk` share under
  `test-hooks` read the real SQLite store, so both gain a test-hook exception to `store-sqlite`.

Measured with `edit_bench --edit crates/store-sqlite/src/command.rs` on the shared Mac, the
incremental state primed:

| `store-sqlite/src/command.rs` | Crates reopened | Test targets reopened | Actions run (rustc / clippy / tests) | Critical path | Edit + build or test |
|---|---:|---:|---|---:|---:|
| Build | 14 | 47 | 35 (22 / 12 / 0) | 26 s | 51 s |
| Test | 14 | 47 | 98 (22 / 12 / 63) | 252 s | 267 s |

- Building meets the phase's goal (≤ 30 s on the critical path).
- Edit + test misses it (≤ 90 s): through the `test-hooks` fixtures above, every test target of the
  engine and the roots still links the store, the scenario shards among them. That run failed only
  on `prose_decisions_test`, whose allowlist still named the line that moved to `domain-rules`;
  it is fixed in the same change.
- Next cut: the engine's shared fixtures stage through `Journal` instead of the SQLite store, so
  their test hooks stop linking the adapter.
