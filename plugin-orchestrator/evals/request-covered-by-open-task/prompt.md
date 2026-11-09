---
name: request-covered-by-open-task
description: A request an open task already covers, worded differently, is folded into that task with no new task
tags: [skill, requests]
runs: 3
max_turns: 20
allowed_tools: [Skill, mcp__plugin_alondra-orchestrator_alondra-orchestrator__title_request, mcp__plugin_alondra-orchestrator_alondra-orchestrator__create_task, mcp__plugin_alondra-orchestrator_alondra-orchestrator__close_request, mcp__plugin_alondra-orchestrator_alondra-orchestrator__send, mcp__plugin_alondra-orchestrator_alondra-orchestrator__set_launch_plan]
append_system_prompt: |
  You are the Alondra orchestrator handling one batch of its inbox. Request effort is auto. The
  project folder is /work/alondra. Act with the Alondra tools; do not answer in prose only.
---

Load the alondra-orchestrator skill, then handle this batch. It is 2026-11-07 15:00 UTC.

Request r-waiting-node, created at 2026-11-07 14:55 UTC by the user:

"Un nodo que espera no debería ocupar memoria. Cuando un nodo está parado esperando mi respuesta,
su proceso sigue vivo y se come RAM del Mac; que lo suelte y vuelva donde estaba cuando conteste."

Tasks in progress (from Alondra, as of this batch), with their missions:
- t-idle-node-release “Cerrar el host y el proveedor de un nodo sin turno”: working, node
  alondra/idle-node-release, /work/alondra. Mission: when a node ends its turn and waits for the
  user, stop its Agent SDK host and provider process; when the user answers, resume the same
  session with the provider's native resume. Approved plan, in progress.
- t-sdk-logs “El host del Agent SDK rota sus registros”: working, node alondra/sdk-logs,
  /work/alondra. Mission: rotate the Agent SDK host's logs at 10 MB; touches agent-host/src/log.ts.

Machine: memory fine, CPU fine, 2 of 4 nodes working. There are no other requests in this batch.
