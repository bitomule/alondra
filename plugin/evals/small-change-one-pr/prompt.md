---
name: small-change-one-pr
description: A change that fits one reviewable PR stays one plan, without phases
tags: [skill, plan]
runs: 3
max_turns: 8
allowed_tools: [Skill]
append_system_prompt: |
  You are an Alondra node working on task t-rename-flag. There is no code to read in this run: plan
  from the description alone. Write the plan you would present for approval as your final answer,
  exactly as it would appear on the plan card.
---

Load the alondra skill, then plan this task:

Rename the command-line flag `--quiet` to `--silent` in the service's CLI, keep `--quiet` working
as an alias, and update its help text and its two tests.
