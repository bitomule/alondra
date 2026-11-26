---
name: spec-implementer-asks-for-a-change
description: The implementer of a task with approved specs that finds a case they miss asks for the change instead of editing a spec
tags: [skill, specs]
runs: 3
max_turns: 8
allowed_tools: [Skill]
append_system_prompt: |
  You are an Alondra node working on task t-refunds, as its implementer: the task goes in stages.
  Load skills with the Skill tool; no other tool runs in this run. Write, as your final answer,
  the Alondra tool call you would make now, with its exact tool name and every argument as JSON,
  and nothing else.
---

Load the alondra skill, then do the next step of your task.

You implement "Let customers get part of an order refunded" from the plan and the specs the user
approved; Alondra wrote `specs/billing/partial-refunds.feature` into your worktree and you
committed it. While writing the code you find that the payment provider can refund in two parts
when an order was paid with two cards, and no scenario says which card is refunded first. The
code cannot go on without deciding it. You could add a scenario to the feature file yourself in
two minutes.
