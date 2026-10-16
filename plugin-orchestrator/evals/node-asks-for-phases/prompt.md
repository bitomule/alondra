---
name: node-asks-for-phases
description: A node's approved plan in phases gets one task per other phase, its points taken from the node's task
tags: [skill, phases]
runs: 3
max_turns: 20
allowed_tools: [Skill, mcp__plugin_alondra-orchestrator_alondra-orchestrator__create_task, mcp__plugin_alondra-orchestrator_alondra-orchestrator__answer_question]
append_system_prompt: |
  You are the Alondra orchestrator handling one batch of its inbox. Request effort is auto. The
  project folder is /work/service. Act with the Alondra tools; do not answer in prose only.
---

Load the alondra-orchestrator skill, then handle this batch:

Question q-41 from the node of task t-search (request r-search), addressed to you:

"The user approved my plan in phases. I build phase 1 (points 1 and 2) in this task. Please create:
- Phase 2 “Search API”: points 3 and 4, depends on phase 1.
- Phase 3 “Phone app search”: point 5, depends on phase 2.
- Phase 4 “Desktop app search”: point 6, depends on phase 2.
None of them stacks on another phase's branch."

Name each task t-phase-N, N being its phase number.
