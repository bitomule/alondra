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
  `OrchestratorAnswers` (`orchestrator.rs`), `SdkHosts` (`hosts.rs`), `AgentHost` (`agent_host.rs`) and `GitHub` (`github.rs`, see "Output adapters"). The values they carry moved with them:
  `LaunchOptions` and `Failed` (`launch.rs`), `NoRoom` (`machine.rs`) and `OutboxMessage`
  (`outbox.rs`). The crates they came from re-export them, so their callers did not change.
- The registry (`Registry`) maps each port's type to its implementation: `provide::<dyn P>(…)`
  gives it, `port::<dyn P>()` asks for it, and `missing()` names every port nothing implements. The
  runtime holds one (`domain.ports()`).
- Each piece that implements a port exposes `pub fn wire(&Registry)`: `daemon-engine`
  (`wiring::wire`), `engine-sdk` (`wiring::wire`), `engine-admission`, `engine-orchestrator` and
  the adapter `github-gh`.
  `alondrad` calls all of them (`domain_bootstrap::wire`), and its test
  `the_daemon_implements_every_port` fails when a port is left without an implementation.
- Every port is called on a `&dyn Runtime`, the context the engine keeps its state in, as
  rust-analyzer's queries take `db: &dyn HirDatabase`. The context says what a piece may ask of the
  kernel: its home, the committed state, a command as Alondra itself, the registry, the wake-up of
  the SDK hosts' owners, a task's launch plan and the extensions (`extensions()`: the state each piece
  keeps in the runtime, by type, in `engine-ports/src/extensions.rs`). `DomainRuntime` implements it. Most implementations are still written
  against `DomainRuntime` and reach it through one bridge, `DomainRuntime::of`; each piece moves to
  the context's methods as it stops depending on the kernel (phase 4), and the bridge then goes.
  Generics would compile each piece into its callers and stop the ports being `dyn` in the registry.
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
- Its tests are `//crates/engine-orchestrator:engine_orchestrator_test`, one target with the real
  hosts and fake Claude and Codex they run. They link neither `daemon-engine` nor `engine-sdk`: they
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
(`machine_watch`). It depends on `engine-kernel` and the crates below it, never on `daemon-engine`,
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
- Until the `test-hooks` builds go (phase 4b), the test binaries that link them (`alondrad`,
  `engine-sdk`, the machine model) take `engine_admission_on_test_hooks`, the same production sources
  built over those builds, as `engine-orchestrator` does.

## The SDK and the launching of tasks

`engine-sdk` is the top of the engine. It holds the owners of the Agent SDK hosts (`sdk_owner`), the
moves of a task to another provider or account (`provider_transition`, `account_move`) and of the
`claude --bg` nodes an earlier release left (`bg_handover`), the automatic launch and its retries
(`auto_launch`, `start_retries`), the scheduled runs (`domain_schedule_runner`,
`domain_schedule_api`), the closing of tasks and the release of resting hosts (`task_closer`,
`idle_release`) and the wiring. It depends on `daemon-engine` and `engine-admission`; only
`alondrad`, the machine model and the scenarios see it.

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
- Each crate of the engine has one test target. The engine's and the gate's tests that start nodes
  through the SDK run in `//crates/engine-sdk:engine_sdk_test`, next to its own, with the hosts, fake
  Claude and Codex and binaries they need; the engine's own target needs only the Node host and a
  fake native Claude, and no test runs on a name filter any more.

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
  of reaching them through the engine's live state; `idle_compaction` re-exports `contexts`.
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

- The engine's libraries and their test-hooks builds depend on `view-model`, never on `views`.
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
  tests: they moved to `engine_sdk_process_test` (`manual`), so relinking the binaries no longer
  reruns the SDK suite.
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
