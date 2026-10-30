---
name: research-mission-plan-or-report
description: A research task's mission says it ends either in the report alone or, when it concludes something should be built, in one plan card with its decisions asked first
tags: [skill, research, plan]
runs: 3
max_turns: 20
allowed_tools: [Skill, mcp__plugin_alondra-orchestrator_alondra-orchestrator__machine, mcp__plugin_alondra-orchestrator_alondra-orchestrator__title_request, mcp__plugin_alondra-orchestrator_alondra-orchestrator__create_task]
append_system_prompt: |
  You are the Alondra orchestrator handling one batch of its inbox. Request effort is auto and no
  request names a model. The project folder is /work/redraft. Act with the Alondra tools; do not
  answer in prose only. When you are done, end your answer with the mission you gave each task you
  created, word for word.
---

Load the alondra-orchestrator skill, then handle this batch. It is 2026-10-30 09:00 UTC.

Request r-index, created at 08:47 UTC:

"Investiga cómo analizan otras apps de fotos toda la fototeca de una vez. Quiero entender si en
Redraft es viable y, si lo es, cómo lo haríamos."

There are no other requests in this batch, and no task is in progress for it.
