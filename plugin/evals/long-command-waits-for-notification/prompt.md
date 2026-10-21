---
name: long-command-waits-for-notification
description: While a long command runs in the background, the node waits for its completion notification instead of polling it
tags: [skill, tokens]
runs: 3
max_turns: 8
allowed_tools: [Skill]
append_system_prompt: |
  You are an Alondra node working on task t-retry-delay, in the git worktree
  /work/repo/.claude/worktrees/t-retry-delay, with the tools Bash, Read, Grep, Edit and Agent. Load
  the alondra skill with the Skill tool: that is the one real call of this run. After it, write as
  your final answer every tool call you would make from now until you know whether the suite passed,
  one per line, as `step N: <Tool> <input>`, and the point where you would end your turn, if any.
  Write nothing else.
---

Load the alondra skill, then go on with your task.

Your change is done and committed. Four minutes ago you started the project's whole test suite,
`./scripts/test-all.sh > .scratch/test-all.log 2>&1`, with Bash in the background (task id
`b7k2m9`); it takes about 25 minutes. Nothing else is left to do on your task until you know whether
it passed.
