# Read side: how screens read the domain

Status: describes `main` on 16 October 2026, after the seven phases of the views-speed plan
(#1014, #1017, #1018, #1021, #1022, #1027, #1030). The rules at the end are also in `AGENTS.md`.

## The shape in one paragraph

The domain changes only by committing a command. Each commit builds, once, a **read index**
(`ReadIndex`, `crates/core/src/domain/read_index.rs`) from the new state and publishes state and
index together as a `ReadSnapshot`. Every screen, the changes feed, the badge, the push gate and
push content read that snapshot plus a **live snapshot** (what nodes, hooks, GitHub and the machine
report, `crates/alondrad/src/views/live.rs`). Nothing on the read side copies the domain, runs a
command to see what it would do, or walks every task or card for each row it draws.

```
command ──▶ DomainState::execute ──▶ SQLite commit ──▶ ReadSnapshot::build(state)
                                                        │  state + ReadIndex, same revision
                                                        ▼
            DomainRuntime::read() ──▶ views::build(key, RenderContext { domain, index, live, … })
                                  ├─▶ changes feed (versions per open view)
                                  └─▶ inbox_badge / waiting_cards / push_gate / push content
```

## The committed snapshot

- `DomainRuntime` (`crates/alondrad/src/domain_runtime.rs`) keeps `snapshot: RwLock<Arc<ReadSnapshot>>`.
  `publish` replaces it after a commit while the store is still held; a replayed command that
  hands back the state already published builds nothing.
- `DomainRuntime::read()` hands out the `Arc` without the store, a transaction or a copy.
- `ReadSnapshot::pair` refuses an index built from another revision or epoch (`stale_revision`):
  a screen never mixes data from two revisions.
- The index is a client read model, never authority: commands, guards and the SQL transaction
  decide; the Agent SDK host does not read it (`docs/agent-sdk-migration/read-model-boundary.md`).

## What the index holds

Relations, by id:

| Accessor | What |
|---|---|
| `tasks_of_request` | the tasks of each request |
| `task_of_node`, `node(..).tasks`, `node(..).all_tasks` | the task a node works for (its latest open one, else its latest), its non-archived tasks, every task it had |
| `cards_of_task`, `cards_of_node`, `cards_of_pr`, `cards_of_document_revision` | the cards of each task, node, PR and document revision |
| `blocked_by`, `blocks` | what holds each delivery back (open cards in front of it, then the upstream delivery of its stack), and which deliveries each open card holds back |
| `task(..).open_blockers`, `task(..).open_dependencies` | the open tasks that hold a task back, and those it still waits for |
| `reviewers`, `dependents`, `prs_of_task` | the tasks that review a task, those that depend on it, the PRs it registered |
| `group_members`, `group_complete` | questions asked together |
| `scheduled`, `failed_launch`, `schedule_title` | tasks created by a scheduled run, the error of a failed launch, the schedule that launched a node |
| `mcp_waiting`, `mcp_waiting_for`, `mcp_wait_of_task` | the open tasks that wait for an MCP server, oldest card first, by server and by task |
| `accounts` | the provider accounts in the user's order, each with its state, email and plan, as the daemon last recorded them (`RecordAccounts`) |

What is derived from the domain alone:

- `TaskReading`: attention, activity, the instruction it was handed back with, a failed merge, its
  cards in Waiting for You, whether it waits for the user or only for a review of its delivery.
- `NodeReading`: phase, whether it merges an authorized PR, its waiting cards, the work it said it
  still does after its PR merged.
- `acting`: the card the user can act on for each task.
- `asks_outside_tasks`: the orchestrator asks the user something that belongs to no task.

The inbox, ordered and decided:

- `queue` (every inbox card in order, with `queue_position`), `waiting` (Waiting for You, each card
  with its buttons), `waiting_card`, `counts` (roots, pending, unread information outside archived
  tasks).
- `item` returns a card outside Waiting for You with its buttons, decided from the index by the
  same rules; `task_actions` gives a task's cancel, reopen and archive buttons.

What the index does **not** hold: text in the client's language and live data (what a node is
doing right now, a pause the machine imposed, PR checks). The view adds those from the
`LiveSnapshot`.

The on-demand rules the index is built from and checked against (`DomainState::inbox_orders`,
`item`, `attention`, `delivery_blocked_by`, `task_actions`…) still live in
`crates/core/src/domain/projection.rs`. The name is historical: `DomainProjection`,
`projection()` and `inbox_projection()` were removed in #1030.

## Buttons: pure guards

`DomainState::guard(actor, envelope)` (`crates/core/src/domain/guards.rs`) judges a command on
`&self`, without copying or changing the state: the envelope checks (protocol, epoch, id, fence,
expected revision), then the command's own rules. `apply` calls the same guard before it changes
anything, so a button and its command share one set of rules. Guards read what holds a delivery
back through the `Holds` trait, which the index implements, so a button is decided without
scanning the cards.

Commands with a guard today: accept delivery, decide a proposed plan, submit feedback, authorize
merge, cancel, reopen and archive a task. Any other command is judged only when it runs.

## Views, feed and notifications

- `views::build(key, RenderContext)` (`crates/core/src/views/mod.rs`) dispatches by `Family`:
  Home, Card, Document, PullRequest, Sidebar, Tasks, Task, Request, Fleet, Node, Schedules,
  Schedule, NewRequest, IssuePicker, Search, Machine. `RenderContext` carries `domain`, `index` and
  `live`.
- The daemon's view cache (`crates/alondrad/src/views/cache.rs`) keeps rendered bodies (256) and
  versions per input — view, client, language, transport, revision and live data (4096). A client
  that already holds a version is answered "unchanged" from there without mounting the view.
- The changes feed compares versions; only views never mounted with those inputs are mounted.
- `home` (iPhone) and `sidebar` (Mac) embed the first three cards of Waiting for You
  (`first_cards`), so the first card arrives with the first answer.
- The badge, card retraction, the push gate, push content and the Mac's notices read the
  committed snapshot; push content is built once per language, not per phone.

## Live data

`live::gather` reads what no command records. Snapshots are kept until a fact they were read from
changes: the domain revision and any row written to the store, hooks, node reports, the usage
pause, the fleet gate, PR details, and the live sampler's clock. Facts are observed before reading
and checked again on every lookup, so a snapshot read across a change is never served. The part
every key shares is kept once per language; keys that read files or GitHub (`machine`,
`new_request`, `issue_picker`) read their own part every time.

## How it is held in place

| Check | Where | Fails when |
|---|---|---|
| Index equals the state's on-demand answers | `assert_index_matches`, after every `execute` in the core tests (`rules.rs`) | the index disagrees on queue order, Waiting for You and its buttons, counts, acting cards, blockers, relations, items, reviewers, dependents, PRs, task buttons, schedule marks |
| Guard equals execution, every button | `assert_guard_matches_execution`, in `action()` under `cfg(test)` | a button's guard accepts or refuses differently from running its command on a copy |
| Guard equals execution, every refusal | `guards_refuse_what_the_commands_refuse` (`domain/tests.rs`) | any button command × 5 actors × 5 envelopes × 4 delivery states disagrees |
| View budget | `//crates/core:views_budget_test` | any of 29 keys renders cold over 50 ms (median of 5) on a real-size domain, or a `Family` has no measured key |
| Index cost | `make read-index-bench` | building the index of a real-size domain with 420 waiting cards takes over 10 ms |
| Snapshot pairing | `domain_runtime.rs` tests | a commit publishes a state without its index, or a stale pair is accepted |
| Live snapshot reuse | `views::live` cache tests | a fact change does not force exactly one new read, or a snapshot read across a change is kept |

`make views-measure DB=<copy>` measures each step on a copy of the real domain
(`sqlite3 -readonly ~/.alondra/domain.db ".backup /path/copy.db"`); it never opens the live file.

## Measurements

Starting point, copy of the real domain on 15 October (alondrad 0.2.81, ~300 tasks, ~280 nodes,
~1730 cards):

| | time |
|---|---|
| `GET /views/home` uncached | 2.9–5.5 s (16.4 s seen on the Mac) |
| one card | 2.2–2.8 s (1.5–9.1 s seen on the Mac) |
| domain projection, per view | 1.2–2.7 s |
| `live::gather`, per request | 150–215 ms |

What each phase changed, as measured in its PR:

| Phase | PR | Measured |
|---|---|---|
| 1. Pure guards | #1014 | inbox projection 1631.6 → 7.4 ms; cold views 1.2–2.1 s → 150–243 ms |
| Live data once per change | #1017 | home (iOS) first/repeat 208.7/185.3 → 12.6/0.6 ms; five cards 1540 → 31 ms |
| 2. Read index per revision | #1018 | index build 0.7 ms on the real copy, 2.2 ms median on the 420-waiting fixture |
| 3. Views read the index | #1021 | real copy, cold `GET /views` 5.3–13.7 → 0.4–6.1 ms |
| 4. Feed and notifications | #1022 | badge 5.3 → 0.00 ms, push gate 10.7 → 0.00 ms, push content 5.1 → 0.13 ms per change |
| 5. Home carries first cards | #1027 | trips to the first card 2 → 1 (172 → 80 ms at 80 ms per relay trip) |
| 6. Projections removed | #1030 | −1958 lines; index build 2.40 → 2.3 ms |

Now, 16 October, `main` at 08a02484:

Copy of the real domain (351 tasks, 304 nodes, 326 requests, 1817 cards, 257 PRs, 1 waiting),
`make views-measure`:

| | ms |
|---|---|
| `ReadIndex::build` | 1.0 median, 1.4 max |
| Waiting for You, counts and acting cards from the index | 0.000 |
| commit one command, index included | 19.0 |
| `live::gather` (home), first after a change | 7.5 |
| `GET /views/home` iOS, cold / warm | 6.8 / 0.5 |
| `GET /views/home` TUI, cold / warm | 0.6 / 0.4 |
| `GET /views/sidebar` Mac, cold / warm | 2.1 / 0.6 |
| `GET /views/tasks`, `fleet`, `task:…`, `node:…`, cold | 0.6–1.2 |
| six card views, total | 3.6 |

Real-size fixture (`views_budget_test`: 286 tasks, 287 nodes, 1858 cards, 270 PRs, 12 waiting):
index build 1.6 ms; every view 0.04–1.4 ms median against the 50 ms budget (sidebar 1.4, search
1.4, the rest under 0.6). `make read-index-bench` (308 tasks, 1708 cards, 420 waiting): 2.4 ms
median, 2.7 ms max against 10 ms.

What is left of a cold home on the real copy is the live gather; the commit cost is SQLite.

## What a daemon start reads

A start (and `alondrad --preflight`, which runs the same start on a snapshot of HOME) opens the
domain once (`DomainRuntime::open`, handed to `DomainDaemon::over`) and builds the first snapshot
from:

- the domain tables (`repository::load_rows`);
- the lifecycle journal of every task: `lifecycle_tasks`, `lifecycle_events` replayed in order,
  `lifecycle_inputs` (each input a task holds pending is looked up once, `persistence::KnownInputs`)
  and the relations a replay checks (`lifecycle_cards`, `lifecycle_dependencies`,
  `lifecycle_delivery`, `lifecycle_current_execution`). Each journal is replayed once
  (`alondra_store::read_journal`): the domain's records are read from each event as it is
  applied (`record_for`), and the lifecycle the replay leaves gives the endings and both journal
  validations (`DomainState::load_journal`, `validate_loaded`);
- `PRAGMA integrity_check` and `foreign_key_check`, which walk every page (66 ms on 64 MB).

Beyond those checks it reads no row of `provider_observations`, `provider_commands`,
`command_audit` or `semantic_receipts`: the SDK owners read their own rows when they adopt an
execution. What a start
costs grows with the journal's events and inputs, and only linearly; `store_test`
`a_replay_looks_up_each_stored_input_once_however_long_it_stays_pending` and `alondrad_test`
`opening_the_domain_replays_a_long_inbox_once_in_linear_memory` hold it. An event costs the
logarithm of the inputs its task holds pending, not their number: the aggregate keeps them in a
persistent queue (`Inputs`, `rpds`), so `apply` copies the state for nothing, and after each event
the replay only looks at the inputs that event names (`Event::inputs`).

### How long raw SDK observations keep their bytes

`provider_observations` holds every event an SDK host sends. Its bytes are read only while the
normalizer has not passed them: the commit that moves the checkpoint replaces the bytes of every
observation below it with one byte (`X'00'`) and keeps the digest, which still tells a copy the
host sends again (`Duplicate`) from a different one (`ObservationConflict`). The checkpoint's own
observation and those not normalized yet keep their bytes; the schema's triggers refuse anything
else. Once an execution is no longer its task's current one, its released rows are removed and
only the checkpoint's observation stays. `retention` runs the bounded pass
(`LifecycleTransaction::retain_evidence`) a minute after start and every hour, and gives the disk
the space back with `PRAGMA incremental_vacuum`; a database still without incremental vacuum is
rebuilt once with `VACUUM`. Nothing in Alondra shows these bytes; to see what a native Claude
said, read its host's `sdk/<instance>/outbox`.

### How long command bodies and receipts are kept

Every commit writes a `command_audit` row (its revision, time, actor, command id and the command's
kind), the whole command in `command_bodies`, and a `semantic_receipts` row (the command's digest
and outcome, so the same command arriving again returns that outcome instead of running twice).
The audit row stays for as long as the domain: the time of a revision is what task history,
`task_state_times`, message and inbox times and `alondra models` read, and `write_mark` reads the
newest revision. The audit row keeps all of an `assign_task`: `launched_by_alondra` reads it for
nodes launched before launches were recorded in `model_decisions`. Nothing reads a body after the
commit. A receipt is read only while the same command can arrive again: app retries are cut before
the domain by one-use ids and an envelope that expires in 120 s, the TUI retries in memory with a
new id per action, the relay queues nothing, and a node retries a card with the same
`interaction_id` right after a failure.

`domain_store/retention.rs` keeps bodies and receipts a week (`WINDOW_MS`) with a list of
per-table policies, each one bounded pass the `retention` loop repeats under the writer lock:
commands older than a week lose their body and their receipt, walking revisions from
`retention_marks`; receipts older than a week go, and those written before receipts carried a time
are dated by the first pass. Bodies live in their own table so that letting them go removes whole
rows: shrinking a row in place leaves its page mostly empty for good. Audit rows written before
that, with the command inside, move their body out once and the store is rebuilt once after. A new
table policy is one more entry in `POLICIES`.

21 October, copy of the real HOME (64 MB `domain.db`, 4974 journal events; one closing task held
1568 pending inputs, 1563 of them the same closer message): preflight 5.15 s → 0.48 s. Before, each
event looked up every pending input again with a freshly prepared statement (1.2 M lookups per
open, the domain opened twice), 81 % of the time.

Later the same day, the same copy (1916 events in that task): loading the domain took 375 ms →
54 ms (median of 20, test build). Before, each open replayed every journal five times, and every
event copied the task's whole queue of pending inputs.

## Rules for new work

1. **A new screen reads the index.** If it needs something the index does not have, add it to
   `ReadIndex` (built in `Builder`, checked in `assert_index_matches`), never a walk over tasks,
   nodes or cards inside the view.
2. **No projections.** Nothing copies the whole domain, or runs a command on a copy, to read it.
   Screens, the feed and notifications read `DomainRuntime::read()`.
3. **A new button has a pure guard and an equivalence test.** Add its command to `guard_given`,
   have `apply` call that guard, build the button through `action()`, and extend
   `guards_refuse_what_the_commands_refuse` with its refusals.
4. **Every family of views is in the budget test.** A new `Family` gets a key in
   `views_budget.rs` `keys()`; `every_family_of_views_is_measured` fails until it has one.
