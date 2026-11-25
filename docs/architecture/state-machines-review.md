# Reviewing a change to a state machine

The `state-machines-review` check of `MUSTS.yml` asks for this review whenever a change touches the
domain, the engine, a store or the lifecycle code. It applies the criteria of the audit that wrote
[state-machines.md](state-machines.md) to the diff alone, so a change cannot bring back the kinds of
failure that audit found. The deterministic part (every machine, state, state file, table, timer and
state enum has its entry) is `//tools:state_inventory_test`; this review is the judgment the guard
cannot make.

## How to review

1. Read the diff. For every changed piece of code that holds, changes, persists, derives or reacts
   to state, find the machine in [state-machines.md](state-machines.md) whose `state-machine` block
   names it (search for the file path or the symbol). A change to a machine the inventory does not
   have yet is a new machine: write its section.
2. Read that machine's section: its source of truth, states, allowed and rejected transitions,
   terminals, invariants, persistence and restart, and concurrency.
3. Answer each question below for the change. Trace each answer through the code to `path:line`
   in the changed tree; an answer that rests on an assumption says so.
4. Update the section, its `state-machine` block and the `state-files` and `timers` blocks so they
   say what the code now does. A finding the change introduces is fixed in the same change, never
   recorded as a new finding to fix later.
5. Record the evidence with `musts evidence`: the machines you reviewed, the answer to each question
   with its `path:line`, and the inventory lines you changed (or why none changed).

## Questions

**1. One source of truth.** Where does the state the change touches live? If the change writes the
same fact to two places (a table and a JSON file, two tables, the store and an in-memory copy), one
of them is the truth and the other is derived from it: say which, and check the derived copy is
rebuilt from the truth after a crash between the two writes, never read as truth. A JSON file
written after a commit, or a table written after a file, is the pattern of F-STORE-4 and F-ENGINE-3.

**2. Allowed transitions.** Which transitions does the change add, remove or move? Each one goes
through the machine's single transition function or command, which refuses what is not allowed with
a typed refusal and leaves the state untouched. A state set by assignment from outside that function,
or a new path that skips its guard, is a finding. When the machine's block declares `transitions`, a new move is a new line there: the domain's
tests fail on a move the block does not declare.

**3. No state without a way out.** For every state a task, request, card, PR, node or owner can now
be in: who moves it next (the user, capacity, another task, the provider, Alondra), and what fact makes
them? A state nobody moves, a card that disappears while its task still waits for it, or a task whose
only way out is a message that may never be sent is the pattern of F-CARDS-1, F-CARDS-4, F-CARDS-5 and
F-STORE-4.

**4. Old answers are not applied again.** Can the change apply an answer, a delivery, a provider fact
or a command twice, late, or out of order: after a retry, a replacement, a restart or a reconnection?
Each must be idempotent or refused by a revision, an id or a seal. An answer to a card that was already
replaced or answered, or a fact about an earlier execution, never changes the current state (F-CARDS-2,
F-CARDS-3, F-GH-2).

**5. What survives a restart.** Stop the daemon at every write the change adds. What does the next start
find, and does it end in a single, valid state without a person's help? An error at startup is handled
by the machine (it retries, repairs or reports), never by stopping the daemon (F-ORCH-5).

**6. Concurrency.** What else runs at the same time on the same state: another thread, a second owner of
the host, the closer, the orchestrator, a client, another process? Check the ordering is enforced by the
serialized write, a lock, or a compare-and-set on a revision, not by timing (F-ENGINE-12).

**7. Time decides nothing a fact should decide.** Does the change add or move a timeout, a deadline, a
poll or a backoff? A deadline that changes a state (frees a slot, declares a host lost, closes a card)
must be declared `decides = "state"` in the timers block with its machine, read the clock only through
`alondra_core::clock`, and be the last resort after the facts that should decide it (F-ENGINE-1). When a
fact exists (the process ended, the provider answered), the fact decides.

## What the review produces

- The inventory section, its block and the timers and state-files blocks match the changed code.
- No question above has an open finding the change introduced.
- Evidence with `path:line` for every answer.
