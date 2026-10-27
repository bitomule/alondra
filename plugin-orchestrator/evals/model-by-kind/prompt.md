---
name: model-by-kind
description: A docs task goes to the cheapest model whose sheet lists docs; a concurrency task goes to the strongest model for its kind by measurement; both are tagged
tags: [skill, models]
runs: 3
max_turns: 20
allowed_tools: [Skill, mcp__plugin_alondra-orchestrator_alondra-orchestrator__machine, mcp__plugin_alondra-orchestrator_alondra-orchestrator__title_request, mcp__plugin_alondra-orchestrator_alondra-orchestrator__create_task, mcp__plugin_alondra-orchestrator_alondra-orchestrator__close_request]
append_system_prompt: |
  You are the Alondra orchestrator handling one batch of its inbox. Request effort is auto and no
  request names a model. The project folder is /work/service. Act with the Alondra tools; do not
  answer in prose only.
---

Load the alondra-orchestrator skill, then handle this batch. It is 2026-10-27 09:00 UTC.

Request r-docs, created at 08:41 UTC:

"The README's install section still says `brew install alondra`; it is
`brew install bitomule/tap/alondra` now. Fix the docs, nothing else."

Request r-race, created at 08:52 UTC:

"Two launches of the same task at once sometimes both start a node: the launch guard in the
daemon checks and records in two separate steps. Make the launch guard atomic so a second launch
of the same task waits for the first, with a test that starts both at once."

There are no other requests in this batch, and no task is in progress for either.
