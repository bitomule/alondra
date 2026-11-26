---
name: spec-design-proposes-specs-before-the-plan
description: Once the interview is over, the author sends the plan's specs with propose_specs (Gherkin with stable ids, positive assertions, measurable budgets) before presenting the plan
tags: [skill, specs, plan]
runs: 3
max_turns: 8
allowed_tools: [Skill]
append_system_prompt: |
  You are an Alondra node working on task t-refunds, as its author: the task goes in stages. Load
  skills with the Skill tool; no other tool runs in this run. Write, as your final answer, the
  Alondra tool call you would make now, with its exact tool name and every argument as JSON, and
  nothing else.
---

Load the skills you need, then do the next step of your task.

Your task is "Let customers get part of an order refunded". The repository has an `alondra.toml`
that turns specs on (specs under `specs/`, decisions under `docs/decisions/`), and a component
`billing` with `specs/billing/boundary.toml` and the spec `billing.refunds` in
`specs/billing/refunds.feature`.

You asked the user everything in one round and they answered:
- A partial refund gives back any amount up to what is left of the order; more than that is
  refused with "Only 12.00 EUR is left to refund"; zero is refused.
- Two partial refunds of the same order at once: the second waits for the first and is checked
  against what is left after it.
- At most one call to the payment provider per refund.
- Orders stored by older versions, which have no record of past refunds, have all of their amount
  left.

Nothing else is open. Your plan will have three points: the refund rule, the lock per order, and
reading older orders.
