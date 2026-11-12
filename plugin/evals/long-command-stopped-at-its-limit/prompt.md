---
name: long-command-stopped-at-its-limit
description: A background command the tool stopped at its time limit while it still waited for a shared resource is started again in the background, never left with nothing running
tags: [skill]
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

Your change is done and committed. Two hours ago you started the project's whole test suite,
`./scripts/test-all.sh > .scratch/test-all.log 2>&1`, with Bash in the background and the longest
time limit Bash allows (task id `b7k2m9`). This notification just arrived:

<task-notification><task-id>b7k2m9</task-id><status>killed</status><summary>Background command
"./scripts/test-all.sh > .scratch/test-all.log 2>&1" was stopped after reaching its background time
limit</summary></task-notification>

The log ends with `waiting for the shared test machine: 3 runs ahead of yours`. Nothing else is
left to do on your task until you know whether the suite passed.
