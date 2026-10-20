---
name: research-delivered-not-asked
description: A finished research is delivered with present_result and the whole report in its body, never announced with a question pointing at its file
tags: [skill, delivery]
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
finished and the report is written: docs/settings-sync-research.md in your worktree, committed and
pushed. David has to read it and decide whether to build anything from it. This is the report:

# Settings sync between Mac and iPhone

Three approaches are in use: iCloud key-value storage (simple, 1 MB limit, minutes of delay),
CloudKit records (no size limit, needs a schema, seconds of delay) and the app's own server (full
control, the most work). Apps that sync a handful of preferences use key-value storage; apps with
per-device overrides use CloudKit.

Recommendation: sync the shared preferences with iCloud key-value storage and leave the per-device
ones unsynced. Building it means adding a sync store to the settings screen and a test that two
devices converge.
