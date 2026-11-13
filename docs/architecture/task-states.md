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

The model below has one visible state per task, one owner for every internal step, three
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

Precedence, first match wins: ended; a card of yours the inbox lists; a live turn or work; your
pause; a standing delivery; waiting, with its reason. `Waiting` without a reason cannot be built.

`Situation` in `crates/core` is that state, computed once from the lifecycle. Labels, blocks,
counters, the Live Activity and the gate's slots all read it; clients never compute a state.

A step of Alondra's own (starting a session, handing an input or an answer, waking a node) holds a
slot of the gate and is not work: the header says how many tasks work, the queue says how many
slots are taken, and both numbers come from the same computation.

## Who owes the next step

`TaskLifecycle::obligation()` names, for every live task, exactly one party and what it owes:

| Owed | Party | The lifecycle is in |
|---|---|---|
| `Start` | Alondra | A session that is opening |
| `HandInput` | Alondra | An input was handed to a session whose provider has said nothing about it yet |
| `ReadInput` | Provider | The provider holds the input (Claude Code `queued`, Codex's answer to `turn/start`) and owes its reading |
| `HandDecision` | Alondra | The user answered a decision an open turn waits on, and the answer has not reached it |
| `Wake` | Alondra | Work of the node's own ended and no turn took its result |
| `Turn`, `OwnWork` | Provider | A turn, or background work, runs |
| `Answer` | Alondra | The orchestrator settles a stopped node, answers a node, or reviews a plan |
| `Merge`, `Close` | Alondra | An accepted delivery merges; a task winds up |
| `Room(capacity)` | Capacity | The gate, the machine, the disk, the usage limit or the provider's API holds it |
| `Tasks` | Other tasks | It waits for tasks or a pull request |
| `User` | The user | A card, a pause or a delivery waits on them |

Only a finished or cancelled task owes nothing.

## Deadlines

A deadline is a last resort. A state that an event of the provider ends carries none, and a
deadline never ends work by itself: it triggers a closed outcome (the turn goes back to the node,
the input goes back to the queue, or the task reads a factual state). It never opens a card for
the user. `obligation_policy` (`crates/core/src/domain/obligation_policy.rs`) is the only place a deadline
may exist, and one clock in the daemon (`obligation_watch`) applies them. `enforced` says which
deadlines are acted on: `HandInput`, `Wake` and `Answer`.

| Step | The event that would make it unnecessary, and why it is not enough | What the deadline triggers | Length |
|---|---|---|---|
| `HandInput`: an input written to a session whose provider has named nothing yet, last resort for a **hung process** | Every provider event ends it: the provider holds the input (`ReadInput`, no deadline), starts it, or refuses it. A refusal (Claude `refused`, `discarded`, or `cancelled` without Alondra's stop; a result that does not name the message; Codex's error on `turn/start` or a turn that ends without our message) is resent at once with a new identifier, and a second refusal hands the message to a host that resumes the same session. A hung process emits nothing by definition | Only when the process is alive and has written nothing at all for the whole length: Alondra stops it, a host that resumes the same session takes its place, and the message goes again flagged as possibly received. Nothing is offered again and no card asks the user | 5 minutes since the process's last output (not since the delivery), and never during a turn or once the provider holds the input |
| `Wake`: the node's own work ended | The turn the provider opens by itself to read that result. Measured over a week on one Mac: it opened in 1,637 of 1,644 cases (median 4.7 s, 99th percentile 49 s); it never does after the host was replaced, because the new process no longer knows that work | Alondra hands the result to the node as an input | 2 minutes: 2.4 times the measured 99th percentile. A false positive only adds an input the open turn reads |
| `Answer`: what the orchestrator owes | Its own call that settles the case, answers the question or reviews the plan. That call is a model's: nothing guarantees it | A stopped node gets its turn back with a default instruction, up to three times, and then its task closes with a result. An unanswered question goes back to its node, which decides or asks the user. An unreviewed plan is released unchanged and its node is told what was decided since | 10 minutes with the orchestrator available. Measured: every stopped node was settled within 48 s, and 95% of 999 questions within 10 minutes |

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

## Rules

Each rule is held by a type or by a property test.

1. **One visible state.** `Situation` is total over tasks, and nothing else decides a label, a
   block, a count or a slot.
2. **Every state has an owner.** `obligation()` is total over live tasks.
3. **A fact of the provider is never refused.** With the right execution and the next cursor, the
   lifecycle applies it or reconciles it. Only a fact about another execution, a repeated fact or a
   gap in the cursor is turned down.
4. **The machine decides.** A caller never tries one variant of an event and falls back to another
   when it is refused; empty turns are counted in one place, the aggregate.
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

A model test (`crates/daemon-engine/src/machine_model`) mixes provider facts, user actions, daemon restarts and
limits, then lets the clock run with nothing else happening, and checks that every open task ends
working with a live session or process behind it, on a card of the user, paused by them,
delivered for their review, or waiting for capacity or tasks that are really missing. It assumes
what the providers document: a running session ends with its result and «idle» or its process
dies and that is seen, and an observation that was not acknowledged is delivered again.

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
| 8 | `Answer` deadline | `orchestrator_inbox` | #167 | Merged |
| 9 | `Situation` | `crates/core/src/domain/situation.rs` | #157 | Merged |
| 10 | Seven state labels | `crates/core/src/views/task_state.rs` | #163 | Merged |
| 11 | One count of work and of slots | `crates/core`, `admission` | #160 | Merged |
| 12 | A present fact on every working row | `crates/core/src/views` (`NowFact`) | #169 | Merged |
| 13 | The wait report | `agent-host`, `crates/alondra`, `docs/wait-report.md` | #171 | Merged |
| 14 | The model test | `crates/daemon-engine/src/machine_model` | #170 | Merged; no known gaps |
| 15 | The legacy node machine retired | `crates/core` `node_machine` | #173 | Merged |
| 16 | An answer always reaches its turn | `admission`, `crates/domain` | #159 | Merged |

Fixes that closed the plan, found by reading it against `main`: #166 (a moved delivery, pause or
task ends correctly), #168 (a returned delivery paused by the disk waits for the disk), #172 and
#176 (a notice left pending after a second delivery or a late answer), #175 (rows change only
when the state does), #178 (the gate reads `Situation`, and «waiting» counts the tasks in «For
you»), #180 (a delivery sent back whose node waits for the orchestrator).

### Where the result differs from the plan

- **A fourth timer remains outside the policy.** `STRANDED_AFTER` (5 minutes, in `admission`)
  sends a task seen stranded back through the gate. It is a deadline outside the policy, so
  «only three deadlines» holds for the policy but not for the daemon; a task will give it an owner
  or remove it.
- **The model test is not fully real.** It reimplements the gate's slots, the host owner and the
  account move instead of calling the code the daemon runs; a task will make it exercise the real
  gate.
- **Some views still read the old state.** The icon and the detail line of a row, and the
  iPhone's header sentence, are not yet derived from `Situation`; a task will move them.
- **Waiting for a cancelled task** advances only when the orchestrator is idle and nothing fails;
  a task will give it an owner and a deadline.
