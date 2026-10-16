---
name: phased-plan
description: A change that does not fit one reviewable PR is planned in phases with real dependencies
tags: [skill, plan]
runs: 3
max_turns: 8
allowed_tools: [Skill]
append_system_prompt: |
  You are an Alondra node working on task t-fast-views. There is no code to read in this run: plan
  from the description alone. Write the plan you would present for approval as your final answer,
  exactly as it would appear on the plan card.
---

Load the alondra skill, then plan this task:

Make every screen of the service fast. Today each screen walks the whole domain on every request.
The work: add a guard for every command so a button can be checked without running it; build a new
read index stored next to the domain; move every screen of the service onto the index; move push
notifications onto the index; expose the index through the public API; update the phone app and the
desktop app to read the new API fields; and delete the old relay routes the apps used before, once
nothing calls them.
