---
name: model-margin-claude
description: An exhausted Codex account does not win a bounded fix over Claude with 65 percent margin
tags: [skill, models, margin]
runs: 3
max_turns: 12
allowed_tools: [Skill, mcp__plugin_alondra-orchestrator_alondra-orchestrator__create_task]
append_system_prompt: |
  You are the Alondra orchestrator handling one batch of its inbox. Request effort is auto and no
  request names a model or restricts the provider. The project folder is /work/parser. Act with
  the Alondra tools; do not answer in prose only. The latest machine snapshot is included in the
  batch below; use it without refreshing it. All MCP tools are mocked.
---

Load the alondra-orchestrator skill, then handle this batch. It is 2026-11-07 09:00 UTC.

Request r-margin, created at 08:47 UTC:

"Fix the parser so an empty input returns an empty list instead of one empty item. Add a regression test; change nothing else."

There are no other requests in this batch, and no task is in progress for it.

Latest machine snapshot:

Available models: model | efforts | account margin | sheet: cost, good for, caution | measured: tasks (done, when some were cancelled), % of the done ones without rework, mean cost; kinds with 3+
claude/sonnet | low,medium,high | Claude 2 65% | medium cost, fix, Small fully specified work | 12: 75% $0.80; fix 12: 75% $0.80
codex/gpt-6.1-sol | low,medium,high | Codex 0% | medium cost, fix/tests/refactor/review/docs, No caution for this bounded task | 4: 75% $1.40; fix 4: 75% $1.40

Account usage (percent used, not remaining):
- Claude 2: week 35%; five-hour window 10%.
- Codex: week 100%; five-hour window 100%.

Machine capacity: memory fine; CPU fine; no nodes working; node limit 4.
