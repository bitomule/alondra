---
name: stale-request-already-covered
description: A request three days old whose work finished and was decided otherwise since is closed in one line, with no task and no question
tags: [skill, requests]
runs: 3
max_turns: 20
allowed_tools: [Skill, mcp__plugin_alondra-orchestrator_alondra-orchestrator__tasks_in_progress, mcp__plugin_alondra-orchestrator_alondra-orchestrator__title_request, mcp__plugin_alondra-orchestrator_alondra-orchestrator__create_task, mcp__plugin_alondra-orchestrator_alondra-orchestrator__close_request, mcp__plugin_alondra-orchestrator_alondra-orchestrator__ask, mcp__plugin_alondra-orchestrator_alondra-orchestrator__send, mcp__plugin_alondra-orchestrator_alondra-orchestrator__set_launch_plan]
append_system_prompt: |
  You are the Alondra orchestrator handling one batch of its inbox. Request effort is auto. The
  project folder is /work/alondra. Act with the Alondra tools; do not answer in prose only.
---

Load the alondra-orchestrator skill, then handle this batch. It is 2026-11-07 09:12 UTC.

Open requests (from Alondra, as of this batch):
- request r-split (opened 3 days ago, no tasks): "Move the website out of the public repository
  and keep only the app and the daemon there."

Finished tasks (from Alondra):
- t-web-private “Move the website to a private repository”: done, PR #41 merged 2026-11-05, it
  removed web/ from the public repository.
- t-web-decision “The whole website stays private”: done, PR #52 merged 2026-11-06, it recorded
  the user's decision of 2026-11-06 that every page of the website lives in the private repository.

Machine: memory fine, CPU fine, 1 of 4 nodes working. There are no other requests in this batch.
