---
name: spec-design
description: How the author of a task designs its specs with the user in a project with specs (an alondra.toml that turns them on) — interview in rounds, find the gaps, turn the answers into Gherkin scenarios, a boundary, decisions and a reference section, and send them with the plan. Use it before presenting the plan of a task in stages whose repository has specs.
---

# Designing the specs of a plan

You are the author of a task in stages, in a repository whose `alondra.toml` turns specs on. The
specs you send with your plan are what the user approves as the truth: once approved, Alondra
writes them into the worktree, a new agent implements against them, and nothing changes them
without the user's approval. Code is regenerated from specs, never the other way round, so a
behaviour no scenario states is a behaviour nobody promised.

Read `docs/architecture/specs.md` of the repository (or its own equivalent) for the formats, and
the specs already under the specs folder of the component you touch: extend them, never repeat
them.

## 1. Size the work first

Pick the level and say it in the proposal (`level`):

| Level | When | Interview | Stages |
|---|---|---|---|
| `none` | The change alters no behaviour: a refactor, documentation, a rename | none | the implementer only |
| `light` | Up to 5 scenarios and low risk | one round | author, implementer, validator |
| `full` | More than 5 scenarios, a new boundary, or a risk of concurrency, security or migration of stored data | rounds until no branch is open; boundary review; decisions | the three, and an adversarial review |

With `none`, send `propose_specs` with no files and present the plan. The user can change the
level on the plan's card.

## 2. Interview in rounds

Each round is one `ask` with every question that can already be answered (up to 4 per card,
with `questions`), each with the option you recommend and why. Facts you find yourself in the
code, the docs and the existing specs; decisions are the user's. Ask again only when an answer
opens something new.

Look for the gaps on purpose; each is a scenario or a decision, never an assumption. The first
round asks about every one of these the request leaves open, stored data and concurrency
included, even when you have a likely answer: recommend it, but let the user decide.

- **Errors and refusals**: what is refused, with what message, and what stays unchanged.
- **Empty and first states**: nothing yet, one item, the limit.
- **Stored data**: what is already saved by older versions and must still read; what the change
  writes and who reads it later.
- **Performance**: a budget a test can measure. Prefer a count (reads, calls, allocations) over
  wall-clock time, which fails on a shared Mac.
- **Concurrency**: two at once, a restart in the middle, an answer that arrives twice. A test
  proves the interleavings it walks, not all of them: name them.
- **Limits**: sizes, counts, lengths, timeouts.
- **Out of scope**: what this change does not do, said so.

Talk to the user in prose, never in Gherkin: explain each spec in a few plain sentences of what
it does and why. That prose becomes the spec's description and its reference section.

## 3. Write the specs

After the last round, turn the prose into files:

- **One `.feature` file per spec**, one observable behaviour at the boundary of a component:
  `@spec:<component>.<name>` above `Feature:`, the explanation as its description, and one
  scenario per case a test can check, each with `@id:<name>` (lowercase letters, digits, dots and
  hyphens). Titles can change; identifiers never do.
- **Steps state exact names, fields, formats and numbers, and assert in the positive**: "Then the
  route `/tasks` answers 201 with the new task's id", never "Then it does not fail". A check an
  empty implementation passes proves nothing.
- **No implementation detail**: no function names, no internal types, no file layout.
- **Performance and concurrency are scenarios too**, with the budget or guarantee a test measures
  ("Then validating 1,000 scenarios reads each file once").
- **A behaviour only a person or an agent can judge** (a screen, a wording) is `@judged`, or
  `@judged:capture` with `@evidence:<pattern>` for committed captures.
- **A new component** gets `specs/<component>/boundary.toml` (what it owns, what it regenerates,
  its inputs and suites) and the scenarios its boundary needs (`@boundary:dependencies`, and
  `@boundary:surface` or `@boundary:data` when it names them).
- **A lasting decision** the specs rest on (a format, an architecture choice, a rule) is a
  decision file with `status = "proposed"`, cited from the spec with `@decision:<id>`; approving
  the plan accepts it. One that should bind future work outside this plan goes on a card of its
  own with `propose_decision`.
- **The reference section** of each spec: the prose you explained it with, under `## <heading>`
  with its `<!-- section: <id> explains: <spec id> -->` line, in the project's reference folder.
- Specs, decisions and reference prose are written in English in a public repository.

Size it to the level: a light change is a few scenarios in an existing spec, never a new
component.

### The files, exactly

A feature file, `specs/<component>/<name>.feature`:

```gherkin
@spec:billing.partial
@decision:refund-what-is-left
Feature: Partial refunds
  A customer gets back any amount up to what is left of the order, and nothing more.

  @id:more-than-left
  Scenario: More than what is left
    Given a paid order of 30 EUR with 18 EUR refunded
    When 20 EUR are refunded
    Then the refund is refused with "Only 12.00 EUR is left to refund"
```

A decision, `docs/decisions/<id>.md` (Markdown with a `+++` header, never a `.toml` file):

```markdown
+++
id = "refund-what-is-left"
title = "A refund never exceeds what is left of its order"
status = "proposed"
date = "2026-11-26"
supersedes = []
superseded_by = ""
+++

## Context
## Decision
## Consequences
## Confirmation
```

Each of the four sections holds its text. A reference section, in `docs/reference/<area>.md`:

```markdown
## How a partial refund works
<!-- section: billing.partial-overview explains: billing.partial, decision:refund-what-is-left -->

The prose you explained the spec with.
```

## 4. Send them with the plan

1. `propose_specs` with `task_id`, `level` and `files` (each the whole new text of a file, or a
   path without text to remove it). Alondra reads them with the spec engine over your worktree
   and refuses the call with every problem by file and line: fix them all and send the whole set
   again.
2. Then present the plan through plan mode. Its checklist names the scenarios each point makes
   true (`spec-engine.scenario-ids/title-change-keeps-id`), so the implementer and the validator
   tie each point to its evidence. Attach a diagram to the plan when it helps the user see how it
   works; it is an explanation, never a spec.
3. The plan's card shows the specs as cards: the user edits them there, comments on a scenario,
   and approves the exact set. Comments come back to you as the request for changes: propose the
   specs again, then present the plan again.

Once approved you end your turn. If the implementer later asks for a change
(`request_spec_change`), a new author starts from that request: draft it with
`propose_amendment` (the whole new text of each file), or `ask` the user first when it needs a
decision. The user decides on its card while the implementer carries on.
