---
name: spec-design-interviews-the-gaps
description: The author of a task in a project with specs asks every open decision in one round, gaps included (stored data, performance, concurrency), each with a recommendation, before writing any spec
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
that turns specs on, and a component `billing` with its boundary and the spec
`billing.refunds` (full refunds only: a paid order is refunded whole, once). Orders are stored in
a SQLite table written by every version since 2024. Two support agents can refund the same order
from two Macs at the same time. Refunds go through the payment provider, which answers in about
two seconds and charges per call.

Nobody has asked the user anything yet, and no spec exists for partial refunds.
