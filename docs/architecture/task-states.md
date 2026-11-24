# Task states

What a task shows, who owes its next step, and what keeps it from standing still. This is the
model the code implements; [Status](#status) says which pull request delivered each part and where
the result differs from the plan.

## Why

A task's state used to be read from five places at once: the lifecycle aggregate
(`crates/domain`, `TaskLifecycle`), the older `TaskState` of the task row, the node's phase
(`NodePhase`), the admission gate's own bookkeeping, and live flags the screens read before the
domain. The screens consulted them in a cascade and produced about 36 state labels and 30 detail
lines. Several of those states could be entered and never left, because the step that ends them
was owed by nobody in particular and nothing checked that it happened.

The model below has one visible state per task, one owner for every internal step, two
deadlines that each say why they exist, and tests that prove no task is left standing.

## The seven visible states

| State | When | Block of the Tasks list |
|---|---|---|
| Working | A turn runs, work of the node's own runs in the background, or the work it declared for after the merge runs | In progress |
| Waiting | It goes on by itself: a slot of the gate (with its place), memory, disk, the provider's usage limit (with its reset time), the provider's API, another task or pull request, or a short step of Alondra's own | Waiting |
| Needs you | A card of yours is open that is not its delivery, and the inbox lists it | For you |
| Paused | You paused it, or interrupted its turn | Waiting |
| Delivered | Its result is presented: for your review, held behind another task, or accepted and closing | For you / In progress |
| Finished | Completed | — |
| Cancelled | Cancelled by you, or closed by Alondra with the reason | — |

Precedence, first match wins (`situation_with` in `crates/domain-rules/src/situation.rs`):
ended; a card of yours the inbox lists; a closed node, which reads as waiting for a slot before
the lifecycle is read; a delivery whose node is closing, which reads as delivered (closing) even
while its last turn runs; a live turn or work; your pause; a card of yours the inbox does not list
yet, which reads as a step of Alondra's; a standing delivery; a cancellation still winding down,
which already reads as cancelled; waiting, with its reason. `Waiting` without a reason cannot be
built.

Every task that has not ended is in exactly one block of the Tasks list, and it never depends on its
request: `task_bucket` in `crates/views/src/tasks.rs` puts an open task in the active list whether its
request is closed, cleared or missing, and a request is closed or cleared only while none of its tasks
is open (the domain reopens it when one starts again). Only an ended task leaves the active list.
`every_open_task_is_in_exactly_one_block_whatever_its_request_says` enumerates every task state against
every request flag.

`Situation` (`crates/domain-rules/src/situation.rs`) is that state, computed once per task in the
`ReadIndex` from the lifecycle, the task's row (a closed node, a cancellation winding down) and the
cards the inbox lists. Labels, icons, detail lines, blocks, counters, the Live Activity and the
gate's slots all read it (`classify` in `crates/views/src/task_state.rs`, `Tally` for blocks and
counts); clients never compute a state.

A step of Alondra's own (starting a session, handing an input or an answer, waking a node) holds a
slot of the gate and is not work: the header says how many tasks work, the queue says how many
slots are taken, and both numbers come from the same computation.

## Who owes the next step

`TaskLifecycle::obligation()` names, for every live task, exactly one party and what it owes.
`Situation` and the clock read it through `DomainState::obligation_of`
(`crates/domain-rules/src/ownership.rs`), which changes it in two cases the lifecycle does not know: a delivery the user sent back while its
node waits on the orchestrator owes `Answer`, and a task no node runs that the orchestrator has to
re-plan owes `Replan`.

| Owed | Party | The lifecycle is in |
|---|---|---|
| `Start` | Alondra | A session that is opening |
| `HandInput` | Alondra | An input was handed to a session whose provider has said nothing about it yet |
| `ReadInput` | Provider | The provider holds the input (Claude Code `queued`, Codex's answer to `turn/start`) and owes its reading |
| `HandDecision` | Alondra | The user answered a decision an open turn waits on, and the answer has not reached it |
| `Wake` | Alondra | Work of the node's own ended and no turn took its result |
| `Turn`, `OwnWork` | Provider | A turn, or background work, runs |
| `Answer` | Alondra | The orchestrator settles a stopped node, answers a node, or reviews a plan. It has no deadline: what the orchestrator owes is handed to it each time a turn of it ends handled (`orchestrator_inbox`) |
| `Replan` | Alondra | A task no node runs waits for a cancelled task, or its launch plan was refused: the orchestrator cancels it, re-points it or lets it start. It is handed the case at each rest, at most three times; past that the task keeps its factual state and no batch is spent on it until what it waits on changes (`obligation_of`, not the lifecycle) |
| `Merge`, `Close` | Alondra | An accepted delivery merges; a task winds up |
| `Room(capacity)` | Capacity | The gate, the machine, the disk, the usage limit or the provider's API holds it |
| `Tasks` | Other tasks | It waits for tasks or a pull request. A task no node runs waits for its dependencies in its lifecycle (`AwaitingTasks`): it enters the wait when created or when a dependency reopens, and leaves it in the command that lands the last one |
| `User` | The user | A card, a pause or a delivery waits on them |

Only a finished or cancelled task owes nothing.

## Deadlines

A deadline is a last resort. A state that an event of the provider ends carries none, and a
deadline never ends work by itself: it triggers a closed outcome (the turn goes back to the node,
the input goes back to the queue, or the task reads a factual state). It never opens a card for
the user. `obligation_policy` (`crates/domain-rules/src/obligation_policy.rs`) holds every deadline
on what a task owes, and one clock in the daemon (`obligation_watch`) applies them. `enforced` says
which are acted on: `HandInput` and `Wake`. What the orchestrator owes has none since #516: it is
settled by state, each time a turn of the orchestrator ends handled.

| Step | The event that would make it unnecessary, and why it is not enough | What the deadline triggers | Length |
|---|---|---|---|
| `HandInput`: an input written to a session whose provider has named nothing yet, last resort for a **hung process** | Every provider event ends it: the provider holds the input (`ReadInput`, no deadline), starts it, or refuses it. A refusal (Claude `refused`, `discarded`, or `cancelled` without Alondra's stop; a result that does not name the message; Codex's error on `turn/start` or a turn that ends without our message) is resent at once with a new identifier, and a second refusal hands the message to a host that resumes the same session. A hung process emits nothing by definition | Only when the process is alive and has written nothing at all for the whole length: Alondra stops it, a host that resumes the same session takes its place, and the message goes again flagged as possibly received. Nothing is offered again and no card asks the user | 5 minutes since the process's last output (not since the delivery), and never during a turn or once the provider holds the input |
| `Wake`: the node's own work ended | The turn the provider opens by itself to read that result. Measured over a week on one Mac: it opened in 1,637 of 1,644 cases (median 4.7 s, 99th percentile 49 s); it never does after the host was replaced, because the new process no longer knows that work | Alondra hands the result to the node as an input | 2 minutes: 2.4 times the measured 99th percentile. A false positive only adds an input the open turn reads |

A turn start is recorded when the provider says the session runs, not when the model's first
message arrives: over a week, that first message took more than 2 minutes in 13% of healthy
starts, so a deadline measured to it would fire on work that is going well.

No deadline, because an event ends the state:

| State | What ends it |
|---|---|
| A running turn | Its result and the provider's «session idle»; the process exiting; the event stream ending without a result, which is recorded as a failed turn. Every wait inside a turn already has the provider's own bound |
| Background work | The provider's list of live work, which is complete each time: what it no longer names has ended |
| An input the host refused before offering it | The refusal is the event: the input is prepared again at once |
| An answer to an open turn | The answer is the event: it reaches its turn as soon as a slot is free, and the gate knows it waits from the state, not because someone asked |
| A move to another account | Each step advances or fails; a failure aborts the move with its reason |
| Capacity, another task, the user | Read again on every pass |

Timers of the engine outside the policy. None of them decides what a task owes; each paces or
bounds the engine's own work, and none opens a card on its own except where the table says so:

| Timer | Where | What it does | Why it is not in the policy |
|---|---|---|---|
| `IN_FLIGHT`, 3 minutes | `crates/engine-admission/src/admission.rs` | A node admitted this recently counts against the Mac's memory before its own use shows. A start whose launch is still under way keeps its slot past it, until its node exists or its launch ends | It estimates memory, not a step anyone owes |
| `OPENING_TIMEOUT`, 2 minutes | `crates/engine-sdk/src/sdk_owner.rs` | A host handed an input that has not opened its session is replaced, after the owner reads what the host already wrote | Owned by the stalled-node recovery work, which decides whether it moves into the policy |
| `RELAUNCH_WAITS` (2 s, 10 s, 30 s, 2 min, then 5 min) and `SILENT_HOSTS` (3) | `crates/engine-sdk/src/sdk_owner.rs` | The wait before each new host that replaces one that ended without taking its input; after three hosts in a row whose Claude never showed it ran, the task holds on the card that says its session could not be resumed | A back-off between attempts and a count of ended hosts, not a time a step may take. It is the one rule here that opens a card |
| `UNOWNED_TAKE_EVERY`, 1 minute | `crates/engine-sdk/src/sdk_owner.rs` | How often the gate tries again to take over a task that reads as working with no host of this daemon | A cadence of retries |
| Idle release, 2 minutes | `crates/engine-sdk/src/idle_release.rs` | Closes the host of a node that rests; its next input starts a host that resumes the session | It frees memory; the task owes nothing while it rests |
| `UNKNOWN_RESET_WAIT_MS`, 30 minutes | `crates/daemon-engine/src/usage_pause.rs` | A usage window reported exhausted with no reset time stops holding starts | It stands in for a reset time the provider did not give |
| `TERMINATE_AFTER` 10 s, `KILL_AFTER` 20 s | `crates/engine-admission/src/pauses.rs` | Signals to the processes of a paused node that do not stop | Bounds a stop Alondra asked for |
| Cleanup wait, `RETRY` 5 minutes | `crates/engine-sdk/src/task_closer.rs` | How long a node may take to clean up before Alondra closes it anyway, and how often a failed close is tried again | Bounds a close Alondra asked for |
| Handoff I/O timeout | `crates/engine-sdk/src/sdk_owner.rs`, `crates/provider-claude/src/adapter.rs` | How long a stopping owner waits for the input or control it hands to the host before asking the host to end | Bounds one frame on the host's socket |

## Rules

Each rule is held by a type or by a property test.

1. **One visible state.** `Situation` is total over tasks, and nothing else decides a label, a
   block, a count or a slot.
2. **Every state has an owner.** `obligation()` is total over live tasks.
3. **A fact of the provider about the current execution is applied or reconciled.** In production
   the guard is the execution's generation (`current_sdk` in `crates/domain-rules/src/lifecycle.rs`),
   the store's cursor and `turn_end_recorded`: a fact about another execution or one already
   recorded is turned down there, before the aggregate. The aggregate itself refuses the facts that
   contradict its state, each with its reason: a repeated or unasked takeover, a lost connection
   after the resource was released, a question with an invalid reason, a turn end that waits on
   itself, a card id already used, and a takeover of a node that is closing. One refused fact
   refuses its whole batch (`record_runtime_facts`).
4. **The machine decides.** Empty turns are counted in one place, the aggregate. Two callers still
   ask the aggregate before they choose an event: the end of a pause tries `EndPause` and records
   `Resumed` when it would be refused or leave the task stranded (`end_of_pause` in
   `crates/domain-rules/src/node_machine.rs`), and a host's end checks that the resume it would
   send is accepted before it records `HostEnded` with it (`record_host_ended`).
5. **An acknowledged command had an effect.** It changed the state, or it repeats a command already
   recorded, or it is refused with the reason.
6. **Every visible state carries a present fact.** `Working` cannot be rendered without one.
7. **A task waits for the user only on a card the inbox lists.**
8. **A closing node only closes.** `DomainState::node_closing` holds once the node confirmed its
   cleanup, the user chose to close it, its end is recorded or the task ended (the node of a
   cancelled task still winds down and may ask the user until it is closed). From then on its
   task reads `Delivered` (closing) even while its last turn is still open, no host takes it over
   (the domain refuses `TakenOver` with `closing`), and nothing it asks becomes a card: a request
   of Claude is denied by policy with the reason, and a card it sends is refused. A host still
   running for a node recorded closed or gone is closed by the gate. A task whose delivery was
   accepted and that was asked to clean up may still ask the user what its cleanup needs, but a
   plan is denied.
9. **A message waits for a paused node.** The orchestrator's `send` to a paused node keeps the
   message with its task's inputs; the pause holds it and the node reads it when it resumes.
10. **The orchestrator's cards are announced after its pass.** A card the orchestrator puts for
    the user during a pass is pushed only once that pass ended, and only if still open.

A model test (`crates/machine-model`) mixes provider facts, user actions, daemon restarts and
limits, then lets the clock run with nothing else happening, and checks that every open task ends
working with a live session or process behind it, on a card of the user, paused by them,
delivered for their review, or waiting for capacity or tasks that are really missing. It assumes
what the providers document: a running session ends with its result and «idle» or its process
dies and that is seen, and an observation that was not acknowledged is delivered again.

The domain, its saved form, the clock (`obligation_watch`) and what the screens and the gate read
are the real code. The gate, the hosts' owners and the account move are modelled at the level of
the commands they record: the model calls their pure decisions (`admission::turns_to_grant`,
`admission::ordered`, `sdk_owner::due_unowned`, `provider_transition::ready_in`, among others) but
keeps its own slots, hosts and in-flight bookkeeping, so a fault in that bookkeeping (as the slot
of a start that expired before its node existed) is outside what it checks.

`KNOWN_GAPS` in `crates/machine-model/src/tests.rs` lists rules the code does not apply yet and the
model applies in its place; it is empty. What the model does not drive at all is listed here and
at the top of `crates/machine-model/src/lib.rs`, so no property covers it:

- interrupting a turn and forcing a node to stop (`Interrupt`, `ForceStop`);
- reopening a task (`ReopenTask`);
- a host released and retaken, an input dropped or held by the host, a turn the provider starts on
  its own, and compaction;
- Codex: every model host is Claude's;
- a held plan review (`SettlePlanReview`), a node's `wait_for_tasks`, work declared for after the
  merge (`DeclarePostMergeWork`), a hold of a task on its provider (`RecordClaudeHold`) and an
  aborted decision.

## What a row says

A task at work says what happens now: the command its node runs and since when, or the oldest
work it left in the background and since when, followed by the last sentence it wrote and its
time. With nothing else to show, it says since when its turn runs.

A node, or the command it runs, may say what it waits for. The node's processes get
`ALONDRA_WAIT_FILE`; a file there holds one JSON object:

```json
{"what": "a build", "progress": "3 of 5", "detail": "behind two test runs", "since_ms": 1794324000000, "pid": 4242}
```

`what` is required; `progress`, `detail` and `pid` are optional. Alondra stores and shows the
text with its time and never interprets it: no field or code path knows any tool. The report lasts
until it is cleared or replaced, its `pid` exits, or the node's turn ends.
`alondra report-wait <what> [--progress …] [--detail …] [--pid …]` writes it and
`alondra report-wait --clear` removes it.

## Status

All sixteen phases of the plan are merged into `main`. The table lists each phase with its pull
request. Where a phase differs from the plan, it is listed below the table.

| Phase | Part | Where | Pull request | Status |
|---|---|---|---|---|
| 1 | `obligation()` on the lifecycle | `crates/domain` | #152 | Merged |
| 2 | One clock, measuring only | `crates/daemon-engine` `obligation_watch` | #154 | Merged |
| 3 | No provider fact about the current execution refused | `crates/domain` `runtime_fact` | #158 | Merged |
| 4 | A move to another account always ends | `crates/daemon-engine` `provider_transition` | #156 | Merged |
| 5 | Turn start, turn end and work end from the provider's session events | `agent-host`, `crates/provider-claude`, `crates/provider-codex` | #161 | Merged |
| 6 | An empty turn goes back to its node at once, up to three times in a row | `crates/domain`, `crates/core` | #162 | Merged |
| 7 | `HandInput` and `Wake` deadlines | `obligation_watch` | #164 | Merged; `HandInput` became the hung-process last resort in the delivery with certainty phases (#185 and its second phase) |
| 8 | `Answer` deadline | `orchestrator_inbox` | #167 | Merged; removed by #516: what the orchestrator owes is settled by state when a turn of it ends handled |
| 9 | `Situation` | `crates/domain-rules/src/situation.rs` | #157 | Merged |
| 10 | Seven state labels | `crates/views/src/task_state.rs` | #163 | Merged |
| 11 | One count of work and of slots | `crates/core`, `admission` | #160 | Merged |
| 12 | A present fact on every working row | `crates/views/src` (`NowFact`) | #169 | Merged |
| 13 | The wait report | `agent-host`, `crates/alondra`, `docs/wait-report.md` | #171 | Merged |
| 14 | The model test | `crates/machine-model` | #170, #182 | Merged; `KNOWN_GAPS` is empty, and what it does not drive is listed under [Rules](#rules) |
| 15 | The legacy node machine retired | `crates/core` `node_machine` | #173 | Merged |
| 16 | An answer always reaches its turn | `admission`, `crates/domain` | #159 | Merged |

Fixes that closed the plan, found by reading it against `main`: #166 (a moved delivery, pause or
task ends correctly), #168 (a returned delivery paused by the disk waits for the disk), #172 and
#176 (a notice left pending after a second delivery or a late answer), #175 (rows change only
when the state does), #178 (the gate reads `Situation`, and «waiting» counts the tasks in «For
you»), #180 (a delivery sent back whose node waits for the orchestrator).

### Where the result differs from the plan

- **Two deadlines, not three.** #516 removed the orchestrator's `Answer` deadline; the policy
  keeps `HandInput` and `Wake`. The stranded-task clock (`STRANDED_AFTER`) was replaced by an
  event in #179: the end of a host owes the start. The engine's other timers pace or bound its own
  work and are listed under [Deadlines](#deadlines).
- **The model test is partly real.** It calls the pure decisions of the gate, the host owners and
  the account move (#182), but keeps its own slots and hosts, and it does not drive the steps
  listed under [Rules](#rules).
- **`Situation` is not read from the lifecycle alone.** A closed node and a cancellation winding
  down come from the task's row, and `obligation_of` adds `Answer` and `Replan` to what the
  lifecycle owes.
- **Waiting for a cancelled task** owes the orchestrator's `Replan` (#553): it is handed the case
  at each rest, at most three times, and a task can no longer be created or re-pointed to wait for
  a task already cancelled.
- **The aggregate has branches no production command reaches.** No command records
  `PlanApproved`, `BlockDelivery`, `DeliveryUnblocked`, `MergeObserved`, `DependenciesResolved`,
  `RetireReleasedExecution` or a lifecycle `Ask`, so `DeliveryGate::Blocked`,
  `DeliveryGate::AwaitingMerge` and `Progress::Dependency` (a started task that waits for others)
  are reached only by the domain's own tests. A task that waits for others before it starts is
  `AwaitingTasks` (#553). The runtime facts `ConnectionLost`, `EffectAccepted`, `EffectObserved`
  and `Notification` are read from stored journals but no adapter emits them.
