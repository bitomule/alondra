---
name: research-ends-in-plan-card
description: A research that concludes something should be built delivers one plan card, the report in content and the work as plan
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
finished, committed and pushed as docs/settings-sync-research.md. You asked Alice which backend
to use and he answered: CloudKit. Nothing else is open. This is the report:

# Settings sync between Mac and iPhone

Three approaches are in use: iCloud key-value storage (simple, 1 MB limit, minutes of delay),
CloudKit records (no size limit, needs a schema, seconds of delay) and the app's own server (full
control, the most work). Apps with per-device overrides use CloudKit.

Conclusion: syncing settings is worth building, with CloudKit (Alice's choice). Building it means
adding a CloudKit sync store to the settings screen, migrating the current settings into it, and a
test that two devices converge.
