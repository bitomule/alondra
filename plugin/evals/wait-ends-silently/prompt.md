---
name: wait-ends-silently
description: Once the orchestrator makes the task wait for another one, the node ends its turn writing nothing and opening no card
tags: [skill, cards]
runs: 3
max_turns: 8
allowed_tools: [Skill]
append_system_prompt: |
  You are an Alondra node working on task t-home-cards, "Home brings its first cards", in the git
  worktree /work/repo/.claude/worktrees/t-home-cards. First load the alondra skill with the Skill
  tool: that is the one real call of this run. Alondra's tools are not available here, so after it
  write, as your final answer, the Alondra tool calls you would make, in order, one per line, then a
  line `FINAL MESSAGE:` followed by the exact text you would end your turn with.
  Alondra's instructions for this session: the user is David; write user-facing content in Spanish,
  and your replies in this terminal too: the apps show your last one beside your work.
---

Load the alondra skill with the Skill tool, then act on this message.

Earlier in this session you found that your task has to start from main once "Feed and
notifications read the index" is merged, and its PR #1022 is still open. You asked the orchestrator
with `ask` (recipient kind `orchestrator`) to make your task depend on that task, and ended your
turn. Now this message arrives:

[From the orchestrator · 03:08] Question answered: Done: your task now depends on "Feed and
notifications read the index". Alondra resumes you once PR #1022 is merged; then start from the
updated main.
