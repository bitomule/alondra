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
`domain-model`, and on `foundation` and `process-group` below them: every piece depends on it, and
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
  `alondrad` calls all of them (`wiring::wire_engine`), and its test
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
  `foundation`. The SDK backend asks Claude Code and Codex their version through `AgentHost::run`
  (`SdkBackend::for_start`), so the kernel starts no process of its own.
- `AgentProvider` and `AgentWorker` (`agent_workers.rs`) are how the engine runs a task's agent on
  its provider: `AgentProvider::configured` gives the worker of one execution, and the provider reads
  its host's frames and records (`conversation_session`, `native_exited`, `account_refusal`…). Each
  provider crate adds itself with its `wire(&Registry)` to `AgentProviders`, which the root
  (`alondrad::wire_providers`) and the engine's test assembly fill; `Registry::missing` names the
  port and each provider left unwired. The values they carry are the providers' common ones
  (`WorkerConfiguration`, `WorkerProfile`, `Retained`, `NativeLiveness`, `Found`).

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
  Claude plays them, and `//crates/provider-codex:codex_host_model` as the Codex host and fake Codex
  play a Codex batch, with no program run. It is the crate's only test target: the record of the
  child the orchestrator owns, which needs real processes, lives in `process-group` (`owner.rs`).
  Each model is held to its real host by a case both run: `provider-claude`'s
  `host_cases/orchestrator_tests.rs` (`sdk_worker_test` and `claude_host_model_test`) and
  `provider-codex`'s `host_cases_tests.rs` (`codex_host_test` and `provider_codex_test`). They
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
  compiles only the tests that need the real Node or Codex host or a fake native Claude or Codex,
  and is end to end: only the release runs it (see "The SDK's tests on the double", "The owner's
  tests on the model" and "The last real-host tests of the SDK"). None carries a binary of the root (see "Tests leave the
  root"), and no test runs on a name filter. Only `engine_sdk_host_test` links the real Antigravity
  provider, for its real-host cases (see "Antigravity out of the engine's per-edit tests").

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
  `host_cases/orchestrator_tests.rs` is the orchestrator's batch: a new conversation, a resumed
  one, one Claude never had and a turn stopped while held, on the real host with the fake
  orchestrator Claude and on the orchestrator model `engine_orchestrator_test` runs on. A model
  that resumed any conversation, or answered another prompt, fails it.
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


## The last real-host tests of the SDK (phase 9, 2026-11-25)

`engine_sdk_host_test` kept 40 tests on real hosts, 32.2 s inside the binary on the shared Mac, its
longest test 27.6 s. Now:

- **The move from `claude --bg`.** `bg_handover`'s tests ran on the pinned Node host and a real
  `/bin/sleep` standing for the node's `claude --bg` process. The move sees that process only
  through `BgSessions`, so the double `Bg` is now an identity and a flag, and the world takes its
  hosts from `crate::engine_tests::SliceHosts`. The Claude host model learnt a session another
  Claude Code ran before any host (`ClaudeModel::ran_elsewhere`, `Hosts::ran_elsewhere`), which a
  host that resumes it starts on. 4 of the 5 run on the model (1.3 s together, about 44 s before);
  the one on a release's installed Claude Code, which runs a real `claude --version`, stays.
- **Three of the owner's tests** pass on the model as they are (a compacted journal, a doubtful
  answer resumed, a live host that refuses its adoption).
- **`a_long_node_keeps_its_host_past_tens_of_mib_and_hundreds_of_inputs` is gone.** Its three
  causes are each tested where they live: the host's journal past 20 MiB and its 256 held commands
  in `agent-host/journal-compaction.test.mjs`, the daemon's 256 tracked commands per connection in
  `agent-protocol/tests/contract.rs` (`duplicate_command_conflict_capacity_and_receipt_phases_are_explicit`).
- **The uncertain takeover** waits out its stopped host's I/O timeout: 4 s instead of 8, the rest of
  the test answering in milliseconds (five runs in a row, 6.9 s each).
- **`engine_sdk_host_test` is end to end**: `manual`, tagged `e2e`, `TEST_EXECS = "e2e"`, in the
  release's `RELEASE_ONLY_TESTS`, out of `MUSTS.yml`. The exec ceiling goes from 215 to 204.
  `TEST_HOOK_EXCEPTIONS` had already left the table (phase 4b).

| Target | Before (main at 36c0386a6) | After |
|---|---:|---:|
| `engine_sdk_host_test`, alone | 40 tests, 32.2 s in the binary | 29 tests, 11.0 s in the binary, 12.7 s the target |
| `engine_sdk_engine_test` | 525 tests | 532 tests, 19.1 s |

The goal of 10 s alone is not met by 1 s. What is left is the Codex host (14 tests, the longest a
thread that opens after its I/O timeout, 7.2 s), the uncertain takeover (6.9 to 8.3 s), the
engine's pause of a delivering task whose hosts end (7.0 s), hosts of an older release, and the
tests of a test process holding, killing or leaving a host, which exist to run real processes.

## Final measurement (phase 11, 2026-11-25, main at d1bcdec12)

`edit_bench --tracked` on the shared Mac, one run, against the goal of one crate's edit: 30 s from
compiling to the last test in adapters, the engine and the root; in the domain, ports and
foundations, compiling ≤ 60 s and in all ≤ 120 s.

| Edit | Layer | Crates / test targets reopened | Build: critical path | Longest test target | Edit + test | Goal |
|---|---|---:|---:|---|---:|---:|
| `views/src/task_state.rs` | adapter | 9 / 50 | 71 s | `scenarios_test` 32 s | 153 s | 30 s |
| `github-gh/src/cli.rs` | adapter | 10 / 46 | 130 s | `scenarios_test` 31 s | 162 s | 30 s |
| `engine-admission/src/admission.rs` | engine | 10 / 51 | 91 s | `engine_sdk_engine_test` 69 s | 190 s | 30 s |
| `engine-orchestrator/src/domain_orchestrator.rs` | engine | 10 / 51 | 85 s | `engine_orchestrator_host_test` 40 s | 174 s | 30 s |
| `engine-live/src/node_reports.rs` | engine | 11 / 54 | 94 s | `scenarios_test` 30 s | 183 s | 30 s |
| `engine-sdk/src/sdk_owner.rs` | engine | 10 / 51 | 95 s | `scenarios_test` 32 s | 178 s | 30 s |
| `alondrad/src/assemble.rs` | root | 8 / 38 | 31 s | `scenarios_test` 31 s | 90 s | 30 s |
| `domain-rules/src/rules.rs` | domain | 42 / 139 | 231 s | `domain_rules_test` 116 s | 436 s | 60 s / 120 s |

No edit meets its goal. What is left above it, measured:

- **Compiling** is most of every edit: 71 to 130 s on the critical path for an adapter or the engine,
  31 s for the root, 231 s for `domain-rules`, whose edit reopens 42 crates. The phases moved tests
  off real processes and out of the root; they did not shrink what an edit recompiles.
- **`scenarios_test`** (about 31 s) is the longest test of five of the eight edits: it links the
  whole daemon, so every crate's edit reruns it.
- **The engine's own targets**: `engine_sdk_engine_test` (69 s in this run, 19 s alone) and
  `engine_orchestrator_host_test` (40 s) run on every edit of the engine.
- **`domain_rules_test`** takes 116 s alone.
- The SDK's real hosts no longer weigh on any edit: `engine_sdk_host_test` is end to end. Edit +
  test on `engine-sdk/src/sdk_owner.rs` went from 247 s (main at 92cc6f1) to 187 s (phase 5) and
  178 s now.

Failures during the run: `//tools:test_execs_test` on every edit (main was red, fixed with this
measurement), `relaunch_test` on four edits and `engine_sdk_engine_test` once, both under the
bench's load. `relaunch_test`'s were macOS's first-launch scan killing daemons whose binary path the
test had already removed or replaced, fixed since (`docs/running-tests.md`, "Executables a test
writes").

## The scenarios without the GitHub client (2026-11-25)

The final measurement above has `scenarios_test` as the longest test of an edit of the `gh` client,
because the scenarios link the root and the root linked `github-gh`. Now:

- The pure readers of what `gh --json` answers (the facts of a PR, which PR answered, the card a
  `gh pr view` makes, the latest run of each check) live in `engine-ports` (`github_json.rs`),
  beside the port, and `Gh` has `set_viewed`, which `github-gh` implements.
- `daemon-server` no longer builds a `GhCli`: the client's reads, marks, closes, PR view, images and
  merger, and the final delivery's public content review, take the GitHub the root registered.
  Its `LAYER_EXCEPTIONS` entry on `github-gh` is gone (`EXCEPTION_CEILING` 53 → 52).
- `alondrad_production` (`crates/alondrad/src/production.rs`), a root library only the `alondrad`
  binary links, holds the real seams (`GithubSeams::production`, `Seams::production`).
  `alondrad_lib`, which the scenarios and the daemon's tests link, no longer depends on `github-gh`.
- The server's and the root's tests register `engine-doubles`' `github::Offline`, a GitHub that
  fails every call as a Mac without `gh` does and reaches no network.

`bazel query 'somepath(//crates/scenarios:scenarios_runner, //crates/github-gh:github_gh)'` is empty;
before it went through `alondrad_lib`. The tests that still link the client run the real binary
(`binary_smoke_test`, `info_commands_test`, `relaunch_test`, the sandbox and system suites) or
drive a scripted `gh` (`engine_github_test`).

`edit_bench --edit crates/github-gh/src/cli.rs`, before (the final measurement above, main at
d1bcdec12, under the bench's load) and after (this change, one run on the shared Mac):

| | Crates reopened | Test targets reopened | Build: critical path | Longest test target | Edit + test |
|---|---:|---:|---:|---|---:|
| Before | 10 | 46 | 130 s | `scenarios_test` 31 s | 162 s |
| After | 6 | 36 | 6 s | `//tools:state_inventory_test` 11 s | 33 s |

- No scenario, `daemon-server` or `alondrad_test` target reruns on an edit of the client any more.
- The two runs were not under the same load (the before's build critical path was 130 s), so the
  time is indicative; the crates and targets reopened are what the graph decides.

## The read index checked in its own property (2026-11-25)

`domain_rules_test` ran `read_index::assert_index_matches` after every transition of every test:
it rebuilds the read index, replays every task's journal twice and compares both with the state's
on-demand answers. A `sample` profile of the slowest property put 81% of its time there; the whole
target spent 248 s of CPU with it and 32 s without, all 868 tests passing either way. The check now
runs after every command of `read_index_tests`, a property over arbitrary histories of the
situation moves plus PRs and their states, work after the merge, information and its reading,
snoozes, archiving, dependencies, problems and merge methods, and after every command of the core
unit tests (`tests.rs`, `card_tests.rs`). With each of these broken, the property fails: a kept
lifecycle never updated after the first record, the work after the merge never cleared on the
task, and a waiting card built without its buttons.

`domain_rules_test` alone, `--report-time`: the run took 31.3 s before and 13.7 s after, and the
test cases summed 229 s before and 50 s after. `edit_bench --edit crates/domain-rules/src/rules.rs`,
same Mac, one run each (load average 20 to 27), before on main at 7b5e782:

| `domain-rules/src/rules.rs` | Crates / test targets reopened | Build: critical path | `domain_rules_test` | Longest test target | Critical path | Edit + test |
|---|---:|---:|---:|---|---:|---:|
| Before | 42 / 139 | 42 s | 129 s | `domain_rules_test` 129 s | 138 s | 207 s |
| After | 42 / 139 | 38 s | 23 s | `engine_orchestrator_host_test` 70 s | 96 s | 188 s |

`//crates/alondrad:relaunch_test` failed in both runs, as in the final measurement above, under the
bench's load.

## Calm by events, one case per shard (phase 3 of the test-time split, 2026-11-25)

95% of each case was the runner's wait for calm: 25 polls of 2 ms with nothing moving, 150 while
an input waited on the owner of a host, which looked again only after up to 250 ms. The polls stood
in for work the clock's count of working threads did not see. Now the count sees it, and the runner
waits for one condition, looked at only when the count comes back to zero or the runtime's thread
goes to sleep (`clock::until_idle`, on the condvar the count already had): nothing works, the
runtime sleeps with no task woken for it, every host is attached and acknowledged, no takeover runs.
No poll and no new timer decides it; `PATIENCE` (20 s) only names a hang, a stretch with no unit of
work starting or ending (a daemon that moves, slowly on a loaded Mac, is waited for), with each
blocking job still running by where it was submitted.

What the count now sees:

- each frame the daemon writes to a Claude host, until the host read it and wrote what it answers
  (`clock::frame_sent` in `provider-claude/src/wire.rs`, `frame_handled` in the doubled host's
  `host_transport::serve`; a host that stops reading drops the rest, and one whose process ended or
  exited, killed by the daemon included, is gone: nothing written to it counts again,
  `clock::host_gone`);
- the daemon's blocking jobs, counted since #650 (`clock::spawn_blocking`); a hang now names each
  one still running by where it was submitted;
- the daemon's async tasks: the runtime's park and unpark hooks and its injection queue;
- an owner of a host that waits for the host while something of its own arrives: `Seals::put`
  rings the bell of each execution the commit issued for, `Seals::wake` (a stop, a slot the
  admission gate gave) rings every bell, and a ring counts the owner as working before it runs.
  In production an input or a control no longer waits up to 250 ms for the owner's next look.

Before removing the polls, every case ran with both criteria at once: after the events said calm,
150 polls of 2 ms had to see no frame and no commit. The first runs named the gaps above one by one
(a blocking job the runner holds at start, the usage probe, counted as work while it waited; async
tasks committed after calm; the admission gate's slot reached the owner only on its next look). With them counted, 28 of the 29
cases kept every checkpoint. `paused-node-coming-back-can-be-paused-again` changed: its pause's
interrupt used to wait for the owner's next look, and the case's resume, under 250 ms later in real
time, cancelled it; now the interrupt leaves at once and the task shows its last turn interrupted.
Removing only the owner's wake restores the old checkpoint (3 of 3 runs); the new one was accepted.
`engine-sdk`'s `an_input_mid_turn_ends_the_owners_wait_for_its_host_at_once` gives the owner an
hour-long look: it passes, and fails on a 10 s timeout without the wake.

`shard_count` is the number of cases (29); the runner fails every shard, naming the count to set,
when they differ.

`scenarios_test --nocache_test_results`, each case's real time, before (main at f0b985ac2, 16
shards, one run) and after (29 shards, two runs):

| Case | Before | After |
|---|---:|---:|
| usage-limit-hours-later-starts-a-new-session | 23.01 s | 1.80 to 1.89 s |
| paused-node-coming-back-can-be-paused-again | 17.33 s | 1.23 to 1.42 s |
| report-held-by-a-changed-repository-asks-where-it-goes | 15.35 s | 0.87 to 1.24 s |
| login-expired-holds-only-its-account | 15.30 s | 1.34 to 2.14 s |
| answer-waiting-for-a-slot-reaches-the-node-after-a-restart | 14.44 s | 1.69 to 2.54 s |
| written-answer-waiting-for-a-slot-reads-as-the-answer | 13.83 s | 0.54 to 1.02 s |
| own-worktree-file-changes-ask-nothing | 13.08 s | 1.81 to 1.93 s |
| integrated-delivery-presented-again-is-a-report | 12.96 s | 1.75 to 1.91 s |
| restart-mid-turn-keeps-the-work | 12.13 s | 1.15 to 1.23 s |
| user-message-survives-a-restart | 11.51 s | 0.99 to 1.60 s |
| claude-api-errors-raise-a-card-only-when-they-persist | 10.40 s | 1.10 to 1.74 s |
| transferred-repository-keeps-one-card | 9.68 s | 1.04 to 1.07 s |
| restart-loses-a-host-and-a-retake-brings-it-back | 9.04 s | 1.53 to 1.69 s |
| manually-opened-pr-is-never-adopted | 8.68 s | 0.75 to 1.32 s |
| provider-outage-opens-no-refusal-cards | 8.27 s | 1.24 to 1.40 s |
| new-session-that-hit-the-limit-goes-on-itself | 8.19 s | 1.68 to 2.46 s |
| stack-undeclared-retargeted-before-merge | 8.11 s | 1.04 to 1.70 s |
| orchestrator-leaves-a-task-question-to-its-node | 7.11 s | 0.65 to 1.18 s |
| merge-waits-for-the-review-github-requires | 6.68 s | 0.71 to 1.20 s |
| usage-limit-closes-merged-delivery | 6.62 s | 0.98 to 1.09 s |
| update-replaces-hosts-it-cannot-adopt | 6.61 s | 0.90 to 1.02 s |
| one-task-refused-while-others-work-asks-after-the-probe | 6.17 s | 1.18 to 1.76 s |
| draft-pr-delivery-refused-until-ready | 6.12 s | 0.78 to 1.27 s |
| answering-one-pr-card-keeps-the-other | 4.79 s | 0.77 to 1.28 s |
| reply-after-login-expired-on-integrated-delivery | 4.59 s | 1.07 to 1.24 s |
| stack-declared-retargeted-before-merge | 4.23 s | 0.73 to 0.91 s |
| reply-to-information-takes-it-out-of-the-inbox | 3.57 s | 0.20 to 0.71 s |
| codex-turn-cut-and-resumed | 0.70 s | 1.07 to 1.54 s |
| output-naming-a-pr-registers-nothing | 0.57 s | 0.76 to 1.02 s |

The 29 cases sum 269.1 s before and 41.4 and 32.5 s after. The test target took
33.8 s (longest shard 23.0 + 6.2 s) before and 6.8 and 5.4 s after; a shard now spends more time
starting and ending its process than in its case.

## Incremental libraries and binaries (2026-11-25, main at b19779773)

The final measurement above left compiling as most of every edit. Profiled with `bazel --profile` on
the shared Mac, one run each, before this change:

| Edit | Build: critical path | The chain of crates that makes it |
|---|---:|---|
| `domain-rules/src/rules.rs` | 237 s | `domain_rules` 28 s → `view_model` 4 → `engine_ports` 5 → `provider_claude` 25 → `daemon_support` 16 → `daemon_engine_model` 8 → `engine_kernel` 20 → `engine_github` 6 → `daemon_engine` 19 → `daemon_server` 74 → `alondrad_lib` 23 → `scenarios` 7 |
| `engine-admission/src/admission.rs` | 118 s | `engine_admission` 5 → `daemon_server` 71 → `alondrad_lib` 28 → `scenarios` 12 |

- **Code generation is the cost, not analysis.** Clippy only checks a crate (`--emit=metadata`, no
  code), so comparing its action with rustc's shows the share: `daemon_server` 38.7 s against 3.9 s,
  `alondrad_lib` 21.1 s against 1.0 s, `cli_mcp` 64 s against 1.9 s, `domain_rules` 21 s against 5.4 s.
  Every crate an edit reopens pays its whole code generation at `opt-level=3`.
- **No early cutoff.** With rustc 1.98, changing only the body of a private, non-inlinable function,
  without moving a line, changes 32 bytes of the crate's `.rmeta`, so Bazel rebuilds everything that
  depends on it. The goal that would fix it, ["relink, don't
  rebuild"](https://goals.rust-lang.org/2025h2/relink-dont-rebuild.html), is not shipped; the [2026
  roadmap](https://goals.rust-lang.org/2026/roadmap-fast-builds.html) puts it in nightly with
  limitations, unfunded.

The levers, measured on the same two edits (an edit is a line in the body of a function; Bazel's
critical path, then the wall clock):

| Lever | `domain-rules` | `engine-admission` | Disk per checkout |
|---|---:|---:|---:|
| As before (`-c opt`, from scratch) | 237 s / 259 s | 103-118 s | 0 |
| `-Copt-level=1` for the repository's crates | 153 s | 66 s (edit + test 221 → 158 s) | 0 |
| Incremental libraries and binaries (adopted) | 62-75 s / 87-96 s | 43 s / 47 s | ~7 GB |
| Incremental libraries, binaries and test crates | 38-48 s / 43-61 s | 17-31 s / 18-46 s | ~15.5 GB |
| Incremental + `opt-level=1` | as incremental | as incremental | |

- **Adopted: libraries and binaries.** `bazel/rules/rust.bzl` gives them `-Cincremental` and
  `no-cache` (an rlib incremental state left incoherent would reach every checkout with the same
  sources). Test crates and third-party crates compile from scratch and stay cached. Release and CI
  (`--//bazel/incremental=false`) compile from scratch. Rustc and Clippy always run locally:
  under `--spawn_strategy=sandboxed` rustc cannot write its state (`could not create incremental
  compilation crate directory … Operation not permitted`). Nothing here compiles sandboxed
  (`--spawn_strategy=worker,local`). The server's two test crates became incremental later (see
  "The server's test crates incremental" below).
- **Not adopted: `opt-level=1`.** It cuts a third without incremental and nothing with it.
- **Not yet: pipelined compilation** (a crate starts once its dependencies' `.rmeta` exists). The bug
  that breaks it unsandboxed on this Mac ([rules_rust#3662](https://github.com/bazelbuild/rules_rust/issues/3662),
  [#4389](https://github.com/bazelbuild/rules_rust/issues/4389)) is fixed by
  [rules_rust#4390](https://github.com/bazelbuild/rules_rust/pull/4390), on its main since 2026-11-17
  and in no release yet (0.75.0, 2026-10-26, is the latest; this repository is on 0.74.0). With
  incremental libraries each crate of the chain costs about a second, so it should save ≤ 5 s an edit.
- **The graph** is phase 7's: the domain's types apart from its rules, and `daemon-server` off the
  engine (71 of the 118 s of an engine edit before this change).

`edit_bench --tracked --profile` after this change, against the final measurement above (main at
d1bcdec12, which edited by appending a static: without incremental compilation any edit rebuilds the
same crates). One run on the shared Mac; every test passed:

| Edit | Layer | Build: critical path, before → after | Edit + test, before → after | What is left on the build's critical path |
|---|---|---:|---:|---|
| `views/src/task_state.rs` | adapter | 71 → 55 s | 153 → 94 s | `views` 2 s → `daemon_server_test` 53 s |
| `github-gh/src/cli.rs` | adapter | 130 → 6 s | 162 → 25 s | `github_gh` 3 s → `engine_github_test` 3 s |
| `engine-admission/src/admission.rs` | engine | 91 → 43 s | 190 → 103 s | `engine_admission` 2 s → `daemon_server_test` 41 s |
| `engine-orchestrator/src/domain_orchestrator.rs` | engine | 85 → 37 s | 174 → 112 s | `engine_orchestrator` 1 s → `daemon_server_test` 36 s |
| `engine-live/src/node_reports.rs` | engine | 94 → 41 s | 183 → 94 s | `engine_live` 1 s → `daemon_engine` 1 s → `daemon_server_test` 39 s |
| `engine-sdk/src/sdk_owner.rs` | engine | 95 → 40 s | 178 → 92 s | `engine_sdk` 3 s → `daemon_server_test` 37 s |
| `alondrad/src/assemble.rs` | root | 31 → 12 s | 90 → 67 s | `alondrad_lib` 4 s → `scenarios_harness_test` 8 s |
| `domain-rules/src/rules.rs` | domain | 231 → 67 s | 436 → 218 s | `domain_rules` 9 s → 7 libraries, 11 s in all → `daemon_server_test` 47 s |

The libraries on the path now take seconds; what is left of compiling is
`//crates/daemon-server:daemon_server_test`, a test crate of 102 files that compiles from scratch on
every edit of the engine or below (36 to 53 s). It is incremental now (next sections). Testing is the
rest of each edit: `scenarios_test` and the engine's own targets.

### The disk it takes

The state lives in each checkout's output base (`rustc-incremental`, `rustc-incremental-clippy`) and
goes with it; deleted without a live Bazel server, it is rebuilt by the next build. Measured from an
empty state over the 139 test targets a `domain-rules` edit reopens:

| | After the first build | After 1 to 5 edits of `domain-rules` (stable) |
|---|---:|---:|
| Libraries and binaries (adopted) | 3.0 GB + 1.1 GB Clippy | 4.9 GB + 2.2 GB Clippy ≈ 7 GB |
| Also test crates | 8.1 GB + 1.1 GB Clippy | 13.3 GB + 2.2 GB Clippy ≈ 15.5 GB |

The first build of a new checkout compiles the repository's libraries and binaries from scratch:
263 s for those 139 targets, against 282 s before for a new checkout and its first `domain-rules`
edit.

The 7 GB is per output base that has built since this change. On 2026-11-25 the external disk held 77
output bases of Alondra checkouts, 56 of them with their checkout still present, and 204 GB free: the
state of all 56 would take about 392 GB, so 7 GB fits while about 29 output bases build (13 with
test crates too). Adding test crates gains 24 to 27 s of critical path on a `domain-rules` edit (62-75
→ 38-48 s) and takes `daemon_server_test` from about 40 s to about 18 s on engine edits, for 8.4 GB
more per output base: worth revisiting once output bases of closed tasks are collected and the live
ones stay few.

## The server's test crates incremental (2026-11-25, main at e4fea67a4)

After the change above, `//crates/daemon-server:daemon_server_test` was most of the build's critical
path on every edit of the engine or below, and `daemon_server_host_test`, the same 102 files with
`real-hosts`, came second. Both compiled the whole server from scratch: the incremental `daemon_server`
rlib took 4 s on an engine edit, the test crates 36-49 s and 19-29 s.

- **Splitting the crate would not reopen less.** Its 698 tests live inside its modules (`service.rs`
  alone holds 48) and every large module (`views`, `service`, `client_routes`, `domain_bootstrap`)
  reaches `daemon_engine` and the `engine_tests` fixtures, so each part of a split still depends on
  the engine and an engine edit reopens all of them. A split pays only if the tests stop recompiling
  the library: out of the crate, onto its public API, each test file rewritten. At best that is the
  incremental `daemon_server` (4-5 s) plus the largest piece from scratch, about what incremental
  gets for a two-line change.
- **Adopted: both test crates incremental** (`incremental_rust_test` in `bazel/rules/rust_test.bzl`:
  rust.bzl's flag and `no-cache`). Off in release and CI like the libraries.

`edit_bench --tracked --profile`, one run of each on the shared Mac; the run after had a load
average of 57, the one before between 3 and 16. Build is the critical path of `bazel build`; edit + test is the
whole `bazel test`. The two server test crates are the compile actions of each in the build:

| Edit | Build, before → after | Edit + test, before → after | `daemon_server_test` | `daemon_server_host_test` | What is left on the build's critical path |
|---|---:|---:|---:|---:|---|
| `views/src/task_state.rs` | 48 → 30 s | 129 → 76 s | 46 → 22 s | 29 → 8 s | `views` 2 s → `views_budget_test` 28 s |
| `engine-admission/src/admission.rs` | 40 → 19 s | 109 → 82 s | 38 → 12 s | 21 → 5 s | `engine_admission` 2 s → `engine_sdk_engine_test` 18 s |
| `engine-orchestrator/src/domain_orchestrator.rs` | 38 → 18 s | 99 → 77 s | 36 → 13 s | 19 → 6 s | `engine_orchestrator` 1 s → `daemon_server` 5 s → `alondrad_lib` 2 s → `scenarios_harness_test` 10 s |
| `engine-live/src/node_reports.rs` | 43 → 21 s | 94 → 81 s | 41 → 14 s | 23 → 5 s | `engine_live` 1 s → `daemon_engine` 1 s → `engine_sdk_engine_test` 19 s |
| `engine-sdk/src/sdk_owner.rs` | 45 → 31 s | 96 → 101 s | 40 → 19 s | 23 → 6 s | `engine_sdk` 5 s → `daemon_server` 6 s → `alondrad_lib` 2 s → `scenarios_harness_test` 17 s |
| `domain-rules/src/rules.rs` | 67 → 82 and 81 s | 205 → 192 s | 49 → 26 s | 27 → 15 s | `domain_rules` 11 s → 3 libraries, 4 s in all → `views_budget_test` 55 s (in the other run, the root's chain to `scenarios_harness_test` 33 s) |
| `github-gh/src/cli.rs` | 4 → 5 s | 28 → 20 s | not reopened | not reopened | `github_gh` 2 s → `engine_github_test` 3 s |
| `alondrad/src/assemble.rs` | 12 → 15 s | 48 → 55 s | not reopened | not reopened | `alondrad_lib` 5 s → `scenarios_harness_test` 9 s |

Neither server test crate is on any critical path now. On an engine edit the build is bounded by
`engine_sdk_engine_test` (18-19 s, from scratch) and by the root's chain to `scenarios_harness_test`;
on `views` by `views_budget_test`, a one-file test crate at `-c opt` (28 s). A `domain-rules` edit gains
nothing yet: `views_budget_test` (40 s before) and the root's chain were already right behind
`daemon_server_test`, and this change touches neither; the build-only run (82 → 81 s) was on a load of
5 to 24. Testing is still most of
each edit: `engine_orchestrator_host_test` (53-71 s) and `engine_sdk_engine_test` are the longest
targets. In the run before, `views_budget_test` (a time budget, on the loaded Mac) and
`scenarios_test` failed once each; the run after passed every test.

**The disk.** Measured in this checkout's output base after the eight edits: `daemon_server_test`
1.4 GB, `daemon_server_host_test` 0.57 GB, 1.9 GB in all, on top of the 7 GB above (Clippy was already
incremental for test crates, so its state does not grow). rustc keeps two sessions of each crate, the
last one and the current; their object files are hard links, but the dependency graph (387 MB) and the
query cache (230 MB) of `daemon_server_test` are written twice, and no stable flag changes that.

**Guards**, each broken by hand and put back:

- `//crates/daemon-server:daemon_server_test_compiles_incrementally_test` and its
  `daemon_server_host_test` twin fail when the target is declared with `low_opt_rust_test` (`Expected
  "["-Cincremental=../../rustc-incremental"]", but got "[]"`).
- `//bazel/incremental/tests:test_compiles_from_scratch_test` fails when `incremental_rust_test`
  passes `-Cincremental` whatever the switch: release and CI compile test crates from scratch.
- `//tools/layers:crate_layers_test` fails when a test with `-Cincremental` lacks `no-cache`, naming
  the fixture and both server test crates.

## Final measurement of the test-time split (phase 4, 2026-11-25, main at 49ac12970)

`edit_bench --tracked --profile` on the shared Mac, one run of each edit, with the read index checked
in its own property (#655), the scenarios without the GitHub client (#654), the scenarios calm by
events (#656) and the orchestrator's tests on the host model (#665). "Before" is the final
measurement of phase 11 above (main at d1bcdec12). #672 (the server's two test crates incremental,
previous section) merged while this ran and is **not** in these numbers.

| Edit | Layer | Crates / test targets reopened, before → after | Build: critical path | Longest test target, before | Longest test target, after | Edit + test, before → after |
|---|---|---:|---:|---|---|---:|
| `views/src/task_state.rs` | adapter | 9 / 50 → 10 / 52 | 71 → 57 s | `scenarios_test` 32 s | `views_test` 18 s | 153 → 115 s |
| `github-gh/src/cli.rs` | adapter | 10 / 46 → 6 / 36 | 130 → 6 s | `scenarios_test` 31 s | `relaunch_test` 16 s | 162 → 26 s |
| `engine-admission/src/admission.rs` | engine | 10 / 51 → 11 / 53 | 91 → 52 s | `engine_sdk_engine_test` 69 s | `engine_sdk_engine_test` 35 s | 190 → 133 s |
| `engine-orchestrator/src/domain_orchestrator.rs` | engine | 10 / 51 → 11 / 53 | 85 → 44 s | `engine_orchestrator_host_test` 40 s | `doctor_service_test` 21 s | 174 → 91 s |
| `engine-live/src/node_reports.rs` | engine | 11 / 54 → 12 / 56 | 94 → 54 s | `scenarios_test` 30 s | `engine_sdk_engine_test` 30 s | 183 → 110 s |
| `engine-sdk/src/sdk_owner.rs` | engine | 10 / 51 → 11 / 53 | 95 → 54 s | `scenarios_test` 32 s | `engine_sdk_engine_test` 67 s, failing | 178 → 137 s |
| `alondrad/src/assemble.rs` | root | 8 / 38 → 9 / 40 | 31 → 15 s | `scenarios_test` 31 s | `doctor_service_test` 18 s | 90 → 53 s |
| `domain-rules/src/rules.rs` | domain | 42 / 139 → 44 / 143 | 231 → 70 s | `domain_rules_test` 116 s | `domain_rules_test` 71 s (`engine_sdk_engine_test` 70 s, failing) | 436 → 220 s |

- **Not all of the drop is the split's.** Between the two measurements libraries and binaries became
  incremental (two sections above), and `edit_bench` changed its edit from an appended static to a
  line in a function. That, more than the split, is what cut compiling. The split's own mark on what
  reopens is the `gh` client (#654); the other counts grew with crates and targets added since. The runs also had different loads: one run each says which target
  is longest and roughly how long, not a difference of a few seconds. Against the `--tracked` table of
  "Incremental libraries and binaries" (94, 25, 103, 112, 94, 92, 67 and 218 s in this table's order)
  edit + test is lower here only on `domain_orchestrator.rs` and `assemble.rs`.
- **The split's two targets no longer lead.** `scenarios_test` was the longest test of five of the
  eight edits and is the longest of none; its longest shard took 10 to 20 s here under the bench's
  load. `domain_rules_test` went from 116 to 71 s, and still leads an edit of the domain (23 s when
  its phase measured it alone).
- **One edit meets the goal**: the `gh` client, 26 s of 30. No other adapter, engine or root edit
  does. The domain compiles in 70 s (goal 60) and takes 220 s in all (goal 120).
- **Compiling** is still the larger part of an engine edit's critical path, mostly
  `daemon_server_test` from scratch (41 to 53 s of the build), which #672 addresses.
- **Failures.** `engine_sdk_engine_test` failed on the `sdk_owner.rs` and `rules.rs` edits, one test
  each time (`a_task_reopened_with_its_node_kept_by_the_user_gets_a_new_execution_that_resumes_its_session`,
  "did not end 2 turns within 60s"), so its 67 and 70 s include that 60 s wait. Alone it passed 3 of
  3 times (main at c5518b711); it is a flaky test to fix. The first run of the `views` edit also
  failed (build 57 s, edit + test 153 s, a `scenarios_test` shard of 32 s) and its log was lost; the
  table has its repetition, which passed.
- **How the rows were read.** The bench was cut by a two-hour limit with six edits measured and only
  printed its table at the end. For `github-gh`, `admission`, `domain_orchestrator`, `node_reports`
  and `rules`, edit + test is bazelqueue's run time of the `bazel test`, as `edit_bench` takes it, and
  the critical path and longest test come from Bazel's profiles, whose test action runs 1 to 2 s over
  the time in Bazel's summary. `sdk_owner`, `assemble` and `views` are `edit_bench`'s own rows.
  `edit_bench` now prints each row as it is measured and keeps the logs of sharded targets, which it
  silently skipped.

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

## The orchestrator's Codex batches in the Codex host model (2026-11-25)

`//crates/provider-codex:codex_host_model` plays the orchestrator's host too: a bootstrap that names
the orchestrator gets the batch `agent-host/codex-session.ts` runs for it (inherited MCP servers and
native tools off, a read-only thread that asks nothing, one turn, the session closed once it
completed), with the outbox and the fake app-server's transcript the model already kept. The three
`orchestrator_codex::policy_integration` tests left `engine_orchestrator_host_test` for
`engine_orchestrator_test`, and the host test no longer runs the Codex host or the fake Codex.

`--report-time`, both targets in one run on the same Mac (load average 11 to 16):

| | Tests | Run | The three Codex tests |
|---|---:|---:|---:|
| `engine_orchestrator_host_test`, before (main at b19779773) | 103 | 26.5 s | 22 s summed, longest 9.9 s |
| `engine_orchestrator_host_test`, after | 100 | 25.4 s | — |
| `engine_orchestrator_test`, after | 61 | 2.6 s | 0.98 s summed, longest 0.67 s |

The host test's run still ends with its longest case, a `recovery_tests::a_write_failure_*` of
25 s here; it falls to the three process tests once the decision tests move to the model.

## The orchestrator's child in `process-group`, and no orchestrator test with real hosts (2026-11-26)

The record of the child the orchestrator owns (`Ownership`, the lock the child inherits, and
`owner.json` with `record`, `end_recorded`, `recorded_run`, `fence_recorded` and
`terminate_orphan`) moved from `engine-orchestrator/src/orchestrator_process.rs` to
`process-group/src/owner.rs`. `process-group` depends only on `foundation`, so an edit of the
domain does not reach it; a test's write faults come in as a closure instead of the orchestrator's
`Injected`. Its three tests with real processes (`/bin/cat`, `/bin/sleep`, a fork held before exec)
run in `process_group_test`. `engine_orchestrator_host_test`, the feature `real-hosts`, its data
(the Node host and the fake orchestrator Claude) and its check go: the orchestrator has one test
target, `engine_orchestrator_test`, on the doubled hosts.

Each host model the orchestrator's tests run on is held to its real host by a case both run, the
same steps and the same expectations:

- Claude: `provider-claude/src/host_cases/orchestrator_tests.rs`, in `sdk_worker_test` (the Node
  host with the fake orchestrator Claude) and `claude_host_model_test`.
- Codex: `provider-codex/src/host_cases_tests.rs`, in the new `codex_host_test` (`agent-host`'s
  Codex host with `fake-codex.py`) and `provider_codex_test` (the Codex host model). It compares
  the events of a batch in order, every native request the app-server got (`thread/start` read-only
  with the inherited MCP server and every native tool family off, `turn/start` at the profile's
  model and effort), the refusal of the app-server's request for approval, and the host ending the
  app-server once the turn completed.

Both cases reach `domain-rules` (through `engine-ports`), so an edit of the domain reruns them: one
test target with real hosts of the orchestrator leaves what a domain edit reruns, and the Codex case
enters it. `bazel query 'rdeps(//..., //crates/domain-rules:domain_rules)'` names no
`engine_orchestrator_host_test` any more.

`edit_bench --edit crates/domain-rules/src/rules.rs --edit crates/engine-orchestrator/src/domain_orchestrator.rs`
on the shared Mac, before on main at c5518b711 (load average 15 to 20) and after on this change
rebased on it (load average 18 to 35):

| Edit | Test targets reopened | Actions run (rustc / clippy / tests) | Build: critical path | Longest test target | Critical path | Edit + build or test |
|---|---:|---|---:|---|---:|---:|
| `domain-rules/src/rules.rs`, before | 145 | 293 (90 / 48 / 151) | 71 s | `domain_rules_test` 90 s | 128 s | 208 s |
| `domain-rules/src/rules.rs`, after | 145 | 289 (90 / 48 / 147) | 53 s | `domain_rules_test` 72 s | 133 s | 236 s |
| `engine-orchestrator/src/domain_orchestrator.rs`, before | 55 | 154 (17 / 9 / 127) | 26 s | `doctor_service_test` 22 s | 64 s | 89 s |
| `engine-orchestrator/src/domain_orchestrator.rs`, after | 54 | 148 (16 / 8 / 123) | 24 s | `doctor_service_test` 19 s | 45 s | 75 s |

- An edit of the orchestrator compiles one crate and runs one test binary less (the host test) and
  takes 89 → 75 s.
- An edit of the domain runs four test actions less; its time did not fall. The orchestrator's host
  test was no longer on its critical path once its decision tests moved to the model (3.3 to 3.9 s),
  `domain_rules_test` is, and the after run met twice the load (it also failed
  `views_budget_test`, a CPU budget, under it). The plan's estimate (edit + test of `rules.rs` from
  188 s to about 150 s) counted the decision tests leaving the host test, which the first phase
  already did; this phase adds no measurable cut to that edit.

## The view budgets and the harness tests without their fixtures (2026-11-26, main at c5518b711)

After the server's test crates became incremental, an edit of the domain or of the screens still
waited for two one-purpose test crates that compiled from scratch.

- **`//crates/views:views_budget_test`** compiles at the build's opt-level, since it measures speed.
  About 450 of its 738 lines measured nothing: they build a synthetic domain and copy it through
  JSON, so the crate generated the code that reads and writes the whole domain. Of its 24,173
  generated functions (`-Zprint-mono-items`), 70% were `serde`'s, and 17 of the 20 s of one compile
  went to LLVM (`-Ztime-passes`).
- **`//crates/scenarios:scenarios_harness_test`** compiled every source of the `scenarios` library a
  second time, from scratch; the same edit already builds that library incrementally. It also sits
  at the end of the chain of libraries, because a scenario runs the whole daemon.

What changed:

- **The budgets' fixture is a test-only library**, `//crates/views:budget_fixture`: the domain, what
  the daemon reads live and every JSON the budgets write or read. It is incremental like every
  library here. The measuring crate holds the same 6 tests and limits and still compiles from
  scratch at `-c opt`. What a limit covers (rendering a view, applying an action and indexing the
  result) did not move. The first-card test serializes its view in the fixture; it prints that time
  and sets no limit on it.
- **The harness's tests are an integration test crate** (`crates/scenarios/tests/harness`) over the
  `scenarios` library, every test it had: 47 when this was measured, 48 with the one `main` added
  before the merge. What they use of it is public now; the library is test-only.

The levers were compared by compiling each crate by hand with Bazel's own `rustc` command, after a
one-line edit of a function body, on the shared Mac (load average 6 to 30):

| `views_budget_test` | Wall | CPU | Disk per checkout |
|---|---:|---:|---:|
| As before, from scratch | 11-17 s | 41-47 s | 0 |
| The same crate, incremental | 0.7-1.7 s | 1.0-1.3 s | ~0.1 GB |
| Fixture apart (adopted): test crate, plus ~0.6 CPU s of the fixture | 1.9-5.3 s | 5.5-8.5 s | ~0.1 GB |

| `scenarios_harness_test` | Wall | CPU | Disk per checkout |
|---|---:|---:|---:|
| As before, from scratch | 7-22 s | 15-23 s | 0 |
| The same crate, incremental | 2.6-5.7 s | 3.7-5.0 s | 0.4 GB |
| Over the library (adopted; the prototype held 20 of the 47 tests) | 1.2-1.8 s | 2.6-2.8 s | 0 |

The incremental budget crate costs less CPU than the fixture library, and either one takes it off the
critical path. The library was adopted because the crate that measures keeps compiling the same way
here, in CI and in a release, and stays cacheable.

`edit_bench --build-only --profile` on both edits, one run of each on the shared Mac (load average
about 17 at the end of the run before, 11 at the end of the run after). The two crates are their
compile actions in the build:

| Edit | Build: critical path, before → after | `bazel build`, before → after | `views_budget_test` | `scenarios_harness_test` |
|---|---:|---:|---:|---:|
| `domain-rules/src/rules.rs` | 68 → 48 s | 103 → 87 s | 54 → 8 s (+ `budget_fixture` 1 s) | 17 → 3 s |
| `views/src/task_state.rs` | 29 → 26 s | 39 → 36 s | 26 → 8 s (+ `budget_fixture` 1 s) | 18 → 5 s |

- What leads the build now: on `domain-rules`, `domain_rules` 13 s → 9 libraries →
  `engine_sdk_engine_test` 24 s; on `views`, `views` 4 s → `daemon_server_test` 22 s.
- The harness now waits for the `scenarios` library (4 to 6 s, built before as well, beside it). The
  whole chain to `scenarios_harness_test` on `domain-rules` read 41 s before and 45 s after: its
  libraries took longer in the second run (`daemon_server` 3 → 11 s, `domain_rules` 11 → 13 s). Its
  own part went from 17 s to 7 s.
- **The budgets measure the same.** The old and new test binaries, built against the same
  libraries, ran 6 times each in turns at load 8 to 10, all passing: every action 58 to 61 ms of CPU
  on both (limit 120), the 37 view medians adding up to 8.3 ms before and 8.0 ms after. The budgets
  count instructions since #681, which this change was rebased on; one run of the test of `main`
  at 6a23342c1 and of this one agreed within 2.5% on every action (614 to 627M against 618 to 637M,
  limit 1,000M) and on every view over 1M, the 37 views adding up to 117.7M and 118.4M.
- **The disk.** The fixture's incremental state took 115 MB, Clippy's included. The harness adds none.
- **The other test crates were left as they are.** The longest chain of compiles in each profile of
  `domain-rules` was recomputed with every `rust_test` compiling in no time, the libraries as
  measured. After this change it falls from 48 to 44 s: the chain of libraries to the root and the
  scenario runner (`domain_rules` 13 s … `daemon_server` 11 s … `scenarios` 4 s) is already that
  long. With the faster libraries of the run before, the same floor is 32 s, but the three longest
  test crates after `views_budget_test` only take the path from 53 to 49 s, and six to 46 s: about
  fifteen of them end within 20 s of each other. The one lever that does not move their tests,
  incremental compilation, does not take a crate near zero (`daemon_server_test`, incremental, took
  22 s in the build after this change), and every test crate incremental takes 8 GB more per checkout
  ("The disk it takes", above).

**Guards**, each broken by hand and put back:

- `//crates/views:budget_inputs_test` fails when `views_budget_test` depends on `serde_json` again
  (`Rustc must not depend directly on these crates … got "["serde_json"]"`), and when it compiles
  the fixture's source (`views_budget_fixture.rs must have test-only presence False`).
- `//crates/scenarios:harness_inputs_test` fails when `scenarios_harness_test` takes the library's
  sources again, naming each one.
- `//tools:low_opt_rust_tests_test` is unchanged: the budgets stay the one `rust_test` at the build's
  opt-level.

## `domain-rules` renamed `domain-model` (phase 7a, 2026-11-26)

The crate that held the domain's types, reads and rules is `crates/domain-model` (`alondra_domain_model`)
now, with every caller, check and catalog entry renamed and nothing else changed: the same sources, the
same 320 test targets. The measurements above keep the name the crate had when they ran.

Phase 7b takes the application of commands and the decisions on cards out of it into a new
`crates/domain-rules`, behind a `DomainRules` port that only the roots wire. The types, the reads and
the guards stay in `domain-model`, because every screen computes its buttons with the same guard as the
command they send (`read-side.md`). A rules edit then reopens the tests that apply commands, not the
about 95 test targets of today's 131 that only read the model.

## The server without `daemon-support` (phase 7e, 2026-11-26, main at a00966012)

`daemon-server` no longer depends on `daemon-support`, and its `LAYER_EXCEPTIONS` entry on it is
gone (`EXCEPTION_CEILING` 53 → 52).

- **Into the server**, the 14 modules only it used (`card_language`, `classify`, `device_usage`,
  `open_files`, `pr_heads`, `pr_ids`, `push`, `relaunch`, `result_files`, `revoked_devices`,
  `scoped_reads`, `start_guard`, `updates`, `workflow_state`) and `auth`, the devices' sign-in: the
  engine only took `random_hex` and `uuid` from it. The upload transfers of `service_files` and
  `refuse_legacy_home` (`legacy_home`) came too: they open SQLite, which nothing below the server
  needs for them.
- **Into `foundation`**: `blocking_work` and `uuid`, which reads the system's entropy with
  `getentropy` instead of linking `ring`.
- **Into `engine-ports`**, what the engine shares with the server and reads or writes on disk:
  `approval_lease`, `domain_cutover` (the marker), `files_retention`, `job_state`, `local_network`,
  `reports`, `stored_files` (the runtime's cache of stored copies and the safe opening of a stored
  path), `uploads` and `watcher`. `local_network` takes the probe it sends with from the root
  (`watch(probe)`); the probe, which finds the router with `netwatch`, is the `system` adapter's.
  `daemon-support` re-exports them, so the engine's imports did not change.

What an edit reopens (`edit_bench --query-only`). An edit of `daemon-support` reopens the same
targets as before this change, so its row is also what editing one of these modules there cost:

| Edit | Crates reopened | Test targets reopened |
|---|---:|---:|
| A module of `daemon-support` (`hook_text.rs`; before, also `updates.rs`, `push.rs`, `auth.rs`, …) | 23 | 90 |
| `daemon-server/src/updates.rs` (any module the server took) | 10 | 49 |
| `engine-ports/src/uploads.rs` (any module moved down to `engine-ports`) | 36 | 123 |

The modules the server took were edited 55 times in the 60 days before this change, the ones moved
to `engine-ports` 25 times: the first now reopen 41 test targets fewer each, the second 33 more.

## The providers behind `AgentProvider` (phase 5f, 2026-11-26, measured at d68285a66)

`engine-sdk` no longer depends on `provider-claude`, `provider-codex` or `provider-antigravity`: it
asks the registry for the `AgentProvider` a task's binding names and runs the `AgentWorker` it
returns. Its three `LAYER_EXCEPTIONS` entries are gone (`EXCEPTION_CEILING` 52 → 49 on today's
main). `SdkBackend::for_start` asks Claude Code and Codex their version through `AgentHost::run`,
so `engine-kernel` starts no process of its own. There is no `sdk-host` crate: the adapter of the
SDK's hosts is `agent-runtime`, behind `AgentHost` since phase 3b.

`edit_bench --profile`, before and after on the same main, one edit each, on the shared Bazel queue:

| Edit | Crates / test targets reopened | Actions (rustc / clippy / tests) | Build: critical path | Edit + build | Longest test target | Edit + build or test |
|---|---:|---|---:|---:|---|---:|
| `provider-antigravity/src/adapter.rs` before | 10 / 44 | 141 (19 / 10 / 111) | 25 s | 42 s | `engine_sdk_engine_test` 28 s | 100 s |
| `provider-antigravity/src/adapter.rs` after | 8 / 40 | 141 (17 / 10 / 113) | 36 s | 52 s | `engine_sdk_engine_test` 37 s | 103 s |
| `provider-claude/src/adapter.rs` before | 24 / 96 | 191 (45 / 24 / 119) | 43 s | 58 s | `provider-claude:sdk_worker_test` 34 s | 138 s |
| `provider-claude/src/adapter.rs` after | 24 / 96 | 218 (45 / 24 / 146) | 38 s | 62 s | `daemon_engine_test` 31 s | 133 s |
| `agent-runtime/src/owned_host.rs` before | 11 / 49 | 143 (18 / 10 / 113) | 17 s | 27 s | `provider-claude:sdk_worker_test` 27 s | 79 s |
| `agent-runtime/src/owned_host.rs` after | 11 / 49 | 144 (18 / 10 / 114) | 16 s | 24 s | `cli-report:cli_report_test` 22 s | 62 s |

- **Antigravity**: in production only `alondrad` links the provider now, so an edit no longer
  recompiles the engine's library. The seconds did not drop: the critical path of both runs is
  compiling `engine_sdk_engine_test` (28–30 s), which links all three providers because its
  Antigravity cases (`sdk_owner/antigravity_tests.rs`) run the real provider through the engine. The
  target of 30 s to build and 90 s to build and test is not met until those cases get a test target
  of their own. The 11 s between the two runs are the shared queue: the same action counts ran.
- **Claude and Codex**: the same 24 crates reopen, because `daemon-engine`, `daemon-support`,
  `engine-live`, `engine-mcp`, `engine-orchestrator` and `store-sqlite` still depend on them
  directly (9 of the 49 exceptions).
- **The hosts' adapter** (`agent-runtime`) stays within the target: 24 s to build, 62 s to build and
  test.

## The server reaches the engine through `DomainApi` (phase 7c, 2026-11-26)

`daemon-server` named three pieces of the engine (`engine-sdk`, `engine-admission`,
`engine-orchestrator`) and held the concrete runtime, so an edit of any of them recompiled the
server library and everything after it.

What changed:

- **The port `DomainApi`** (`engine-ports/src/domain_api.rs`) bundles four `dyn` parts: the
  runtime's operations (`daemon-engine/src/runtime_api.rs`), schedules and the SDK
  (`engine-sdk/src/sdk_api.rs`), admission and the orchestrator. Each piece gives its part to the
  registry in its `wire`; the server takes the bundle from the registry and holds an
  `Arc<dyn Runtime>`, not `Arc<DomainRuntime>`, in `DomainDaemon`, `Client`, `Service`,
  `PushAlerts` and the push loops. The schedule routes' values those parts take and return moved
  down to `engine-ports` (`schedule_routes`).
- **The root opens the daemon.** `wiring::wire_engine`, the opening of the store, `--preflight` and
  the sidecar's migration helpers moved from the server to `alondrad`.
- The server's two test crates still link the engine, as tests (`test_visibility` on the engine's
  crates).
- The layer exceptions fall by three (`EXCEPTION_CEILING` 49 → 46, on top of phases 7e and 5f); the server
  keeps only `daemon-engine` (phase 7d).

`edit_bench --profile --edit crates/engine-orchestrator/src/domain_orchestrator.rs` on the shared
Mac, before on main at b6a2fbd49 (load average 20 to 21) and after on this change on the same
commit, both before phase 7e and the `domain-model` rename reached main:

| Edit | Crates reopened | Test targets reopened | Actions run (rustc / clippy / tests) | Build: critical path | Edit + build | Longest test target | Critical path | Edit + build or test |
|---|---:|---:|---|---:|---:|---|---:|---:|
| `engine-orchestrator/src/domain_orchestrator.rs`, before | 11 | 55 | 158 (16 / 8 / 133) | 19 s | 31 s | `doctor_service_test` 19 s | 50 s | 96 s |
| `engine-orchestrator/src/domain_orchestrator.rs`, after | 10 | 53 | 157 (15 / 8 / 132) | 18 s | 29 s | `doctor_service_test` 22 s | 41 s | 78 s |

- The edit no longer recompiles the server library: the build's critical path was
  `engine_orchestrator` 2.2 s → `daemon_server` 6.1 s → `alondrad_test` 10.0 s and is
  `engine_orchestrator` 1.9 s → `daemon_server_test` 15.1 s, the server's test crate, which links
  the engine on purpose.
- Edit + build or test: 96 → 78 s. Most of the cut is the critical path (50 → 41 s); the longest
  test target, `doctor_service_test`, does not depend on the server and took 19 and 22 s.

**Guards**, each broken by hand and put back:

- `//tools/layers:crate_layers_test` and `//crates/daemon-server:daemon_server_layer_test` fail in
  analysis when the server depends on any of the three again (`//crates/daemon-server:daemon_server
  depends on //crates/engine-orchestrator:engine_orchestrator: a crate of layer adapter-in may
  depend only on layers foundation, domain, ports`).
- `alondrad`'s `the_daemons_wiring_alone_implements_every_port` fails when a piece leaves its part
  out of its `wire` (`a port the daemon's wiring leaves to nothing … ["OrchestratorApi"]`).

## The rules behind the `DomainRules` port (phase 7b, 2026-11-26)

What a command changes left `domain-model` for a new `crates/domain-rules` (`alondra_domain_rules`):
the big `apply` of every command, the passes every command runs after it, the decisions on SDK cards
and the halves of `node_machine`, `claude_access`, `stage_rules`, `account_settings`, `mcp`,
`mcp_wait`, `compaction` and `account_login` that change the state, about 6,400 lines. Rust lets no
crate add methods to a type of another, so they are functions of the rules crate over
`&mut DomainState`.

`domain-model` keeps the types (with the same serde format), every read, the guards (`guard`,
`guard_given`, `admit` and each command's own), `ReadIndex` and `projection`, the journal's codec
and its replay, and the lifecycle authority: `decide` and `judge` still judge every record, and what
they change on the way (closing a task, opening a stall's case, retiring a card, deriving the
states) stays with them. Loading a stored state applies no rule, so the store loads without the
port.

The port is `DomainRules` in `domain-model`: `transition` applies a command to a copy of the state
and `anticipate` says what a command would do without applying it. `domain_rules::Rules` implements
it. The root hands it to the store when it opens it (`alondra_store_sqlite::Sqlite(&Rules)`, in
`alondrad` and its preflight), the store applies every command with it, and `engine-sdk`'s closer
asks the store's `Journal` for it to anticipate a node's end.

`domain-rules` has a layer of its own, `domain-rules`, that only the roots may depend on: a production crate
of any other layer that links it fails the analysis (`crate_layers_test`). The tests that apply
commands link it: those that open a store, and those that run commands on a state directly (`views`,
`cli`, `system`, `machine-model`, `engine-doubles`).

Nearly every test of the old crate builds its state by running commands, so 50 test files moved to
`//crates/domain-rules:domain_rules_test` with their names; `//crates/domain-model:domain_model_test`
keeps the tests that need no rule (the card codecs, the checklist and file parsers, the schedule).
Every button the rules' tests project is still checked against its command
(`check_every_button`): the model calls a check the rules' tests install.

`edit_bench --profile --edit crates/domain-rules/src/rules.rs`, one run on the shared Mac (load
average about 13 to 20). Before is the same edit of `domain-model/src/rules.rs` measured on 26-11
for the plan of this phase:

| Edit of the rules | Crates reopened | Test targets reopened | Build: critical path | Edit + build or test | Longest test target |
|---|---:|---:|---:|---:|---|
| Before (`domain-model/src/rules.rs`) | 44 | 131 | 48 s | 192-220 s | - |
| After (`domain-rules/src/rules.rs`) | 12 | 68 | 50 s | 179 s | `domain_rules_test` 48 s |
| Goal of the phase | - | about 36 | ≤ 30 s | ≤ 120 s | - |

- **What it reopens.** 12 crates: the rules and the roots that link them. 41 of the 68 test targets
  reach the rules through `deps`: the ones that open a store or run commands on a state (the plan
  counted 36; `cli`, `cli-tui`, `system`, `domain_screens_test` and `read_index_cost_test` run
  commands too). The other 27 read the edited file as data, the guards over the repository's
  sources (prose decisions, the inventory, the log catalog …), which every edit of any Rust source
  reopens.
- **The build's critical path did not fall.** It was `domain_rules` 13 s → 9 libraries →
  `engine_sdk_engine_test` 24 s; it is now `domain_rules` 6 s → `store_sqlite_test` 43 s. The
  libraries left the path, but the test crates that apply commands compile from scratch, and
  `store_sqlite_test`, the largest of them, takes the place of the chain. The goal of 30 s needs those
  test crates faster (incremental, or split as the budgets were), which this phase does not do.
- **Edit and tests**, 179 s, under 192-220 s but over the goal of 120 s. The longest test target is
  `domain_rules_test` (48 s), which now holds almost every test of the old crate.

## Antigravity out of the engine's per-edit tests (2026-11-26, main at b2d1db1a5)

The engine's test assemblies no longer wire the real Antigravity provider:

- `engine-sdk`'s `model_fixture::open`;
- `engine_assembly::open_engine_at`, which the tests of `daemon-server` compile too;
- `daemon-engine`'s `engine_alone`;
- the test of a task whose provider nothing wired.

None of their tests runs an Antigravity node. Its cases (`sdk_owner/antigravity_tests.rs`) need the
real host and the fake `agy`, so they compile only with `real-hosts`, and `engine_tests::open_at`
wires the provider there.

`provider-antigravity` left the deps of `engine_sdk_test`, `engine_sdk_engine_test`,
`daemon_engine_test`, `daemon_server_test` and `daemon_server_host_test`. Only `engine_sdk_host_test`
(manual, end to end) and the root link it. `TESTS_KEPT_APART` keeps it out of those five: with the
dependency back, `crate_layers_test` fails with `//crates/engine-sdk:engine_sdk_engine_test depends
on //crates/provider-antigravity:provider_antigravity again`. `test_catalog -- compare` over the
three packages finds the same 1,740 tests in the same targets.

`edit_bench --profile`, before and after on the same main, on the shared Bazel queue:

| Edit | Crates / test targets reopened | Actions (rustc / clippy / tests) | Build: critical path | Edit + build | Longest test target | Edit + build or test |
|---|---:|---|---:|---:|---|---:|
| `provider-antigravity/src/adapter.rs` before | 8 / 40 | 145 (17 / 10 / 117) | 26 s | 35 s | `engine_sdk_engine_test` 27 s | 89 s |
| `provider-antigravity/src/adapter.rs` after | 8 / 30 | 124 (12 / 5 / 106) | 11 s | 15 s | `alondrad:relaunch_test` 19 s | 60 s |

- **Target met:** 30 s to build and 90 s to build and test an Antigravity edit. The build's
  critical path is now compiling `alondrad_test` (9 s), which links every crate the daemon does.
- **Load:** the before row is a quiet queue. On 2b43794b2, under memory pressure, the same edit
  measured 43 s and 99 s.
- **Codex:** its cases share the model hosts' fixtures with the rest of the engine's tests
  (`model_fixture/launch_fixture.rs`, `engine_fixtures/model_hosts.rs`). They get a target of their
  own in phase 4 of the plan that empties `LAYER_EXCEPTIONS`, when `daemon-engine` stops linking Codex.

## The engine's shared data in the ports layer (phase 2 of emptying the exceptions, 2026-11-26)

`daemon-support` and `daemon-engine-model` moved to the `ports` layer, and the 21 exceptions of the
crates that depended on them went with it (the ceiling of `LAYER_EXCEPTIONS` from 46 to 25,
[layers.md](layers.md)). Before the move each lost what tied it to a layer above:

- `only_text_field` and `elicitation_content` live in `core` (`model::elicitation`), next to the
  answer options: the hook texts and both providers use that one copy, Codex lost its own, and
  `daemon-support` no longer depends on `provider-claude`.
- The schedule runner's wall-clock timers (the dispatch timer and the clock and time zone
  notifications) are the `system` adapter's (`schedule_clock::MacClock`), behind the
  `ScheduleClock` port of `engine-ports`. `system::wire` gives it to the registry and the runner asks
  the registry for it; `engine-doubles` has a clock a test moves by hand.
- The HTTP side of a reply (`IntoResponse`, `Reply::error` and `status_for`) is `daemon-server`'s
  (`reply.rs`): the model keeps only the `Reply` value, on the `http` crate's status instead of
  axum's, and the engine's relayed messages answer with the domain's refusal, which the server
  turns into the status.
- `daemon-engine-model` takes `locks` and `save` from `foundation`, so it no longer depends on
  `daemon-support` at all.

`edit_bench --query-only`, before on main at 2e6e5b1 and after on this change, same Mac (load
average about 10 to 17):

| Edit | Before: crates / test targets reopened | After |
|---|---:|---:|
| `daemon-support/src/hook_text.rs` | 23 / 90 | 22 / 87 |
| `daemon-support/src/schedule_clock.rs` → `system/src/schedule_clock.rs` | 23 / 90 | 10 / 43 |
| `daemon-engine-model/src/launch.rs` | 22 / 87 | 22 / 87 |
| `daemon-engine-model/src/reply.rs` | 22 / 87 | 22 / 87 |
| `system/src/machine.rs`, for reference | 10 / 43 | - |
| `engine-ports/src/schedule_clock.rs` (the new port) | - | 36 / 123 |

- **The clock's code** costs what any edit of `system` costs: only the roots link it.
- **`hook_text.rs`** reopens one crate less: `daemon-engine-model` no longer depends on
  `daemon-support`. The rest of the reclassified crates' edits cost the same, as the plan expected:
  a layer is a rule about who may depend on whom, not a change of the graph.
- **The port** is in `engine-ports`, whose every edit reopens 36 crates; it is a trait of two
  methods that should rarely change.

## The server without the engine (phase 7d, 2026-11-26)

`daemon-server` still depended on `daemon-engine`, for about 140 of its items: the live view (the
nodes the hooks deduced, what each node's mod reported, the contexts, the waits), the accounts, the
messages to nodes, the orchestrator's tools, the pull requests and the MCP servers. An edit of any
piece under `daemon-engine` recompiled the server library and everything after it.

What changed:

- **Six more parts of `DomainApi`** (`engine-ports/src/domain_api.rs`): `LiveApi`, `AccountsApi`,
  `MessagesApi`, `ToolsApi`, `PullRequestsApi` and `McpApi`, plus four operations of `RuntimeApi`
  (closing a session, cleanup, the rules «allow always», a system command). `daemon-engine`
  implements them in `server_api.rs` and gives them to the registry in its `wire`; the registry's
  list of required ports names them, so a wiring that leaves one out fails at start.
- **The values they speak in moved to `engine-ports`**, re-exported where they were so the engine
  reads them unchanged: `live_view` (a node's report as the screens read it, the waits, the runs and
  steps, `/runtime`'s nodes), `mcp` (the checks and removals, the targets of a sign-in), `pr_images`,
  `options`, `issues`, `settings_file`, `local_actions`, the machine's gauges and `IntoDomainError`.
- The server keeps its own route `Reply`; the HTTP status of a domain refusal has one table,
  `domain_api::status_code`.
- `daemon-server` has no layer exception left (`EXCEPTION_CEILING` 25 → 24, after the 21 the ports
  layer took away). Its two test crates
  still link the engine, as tests.

`edit_bench --profile --edit crates/engine-live/src/node_reports.rs` on the shared Mac, before on
main at 3264efd4d (load average 15) and after on this change (load average 6 to 14):

| Edit | Crates reopened | Test targets reopened | Actions run (rustc / clippy / tests) | Build: critical path | Edit + build | Longest test target | Critical path | Edit + build or test |
|---|---:|---:|---|---:|---:|---|---:|---:|
| `engine-live/src/node_reports.rs`, before | 12 | 59 | 155 (19 / 10 / 124) | 26 s | 35 s | `engine_sdk_engine_test` 35 s | 72 s | 107 s |
| `engine-live/src/node_reports.rs`, after | 11 | 57 | 151 (18 / 10 / 121) | 31 s | 41 s | `engine_sdk_engine_test` 31 s | 70 s | 100 s |

- The edit no longer recompiles the server library: one crate and two test targets fewer.
- The seconds barely move, as the plan expected for a local build with incremental libraries: the
  critical path of both runs is `engine_live` → `daemon_engine` → `engine_sdk_engine_test`, a test
  crate compiled from scratch (21.9 s before, 27.2 s after, the shared queue's noise). The gain is
  the server library's compile in a release or CI build from scratch, and the last of its
  exceptions.

**Guards**, each broken by hand and put back:

- `//tools/layers:crate_layers_test` and `//crates/daemon-server:daemon_server_layer_test` fail in
  analysis when the server depends on `daemon-engine` again (`//crates/daemon-server:daemon_server
  depends on //crates/daemon-engine:daemon_engine: a crate of layer adapter-in may depend only on
  layers foundation, domain, ports`).
- `alondrad`'s `the_daemons_wiring_alone_implements_every_port` fails when `daemon-engine` leaves a
  part out of its `wire` (`left: ["LiveApi"]`).
