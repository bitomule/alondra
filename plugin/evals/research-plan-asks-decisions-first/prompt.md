---
name: research-plan-asks-decisions-first
description: A research that ends in a plan with an open decision asks it first, never delivers a plan card with options inside it
tags: [skill, delivery, plan]
runs: 3
max_turns: 8
allowed_tools: [Skill]
append_system_prompt: |
  You are an Alondra node working on task t-sync-research. Load skills with the Skill tool; no
  other tool runs in this run. Write, as your final answer, the Alondra tool call you would make
  now, with its exact tool name and every argument as JSON, and nothing else.
---

Load the alondra skill, then do the next step of your task.

Your task is "Research how other apps sync settings between Mac and iPhone". The research is
finished, committed and pushed as docs/settings-sync-research.md. This is the report:

# Settings sync between Mac and iPhone

Three approaches are in use: iCloud key-value storage (simple, 1 MB limit, minutes of delay),
CloudKit records (no size limit, needs a schema, seconds of delay) and the app's own server (full
control, the most work).

Conclusion: syncing settings is worth building. Two ways fit our app equally well and the choice
is Alice's: key-value storage (ships in a day, but per-device overrides are lost) or CloudKit
(three days, keeps per-device overrides). The plan you would propose: add a sync store to the
settings screen with the chosen backend, migrate the current settings into it, and a test that two
devices converge. Nobody has asked Alice about the backend yet.
