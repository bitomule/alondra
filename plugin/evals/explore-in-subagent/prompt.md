---
name: explore-in-subagent
description: Broad code exploration across a flow goes to a subagent that returns only its conclusion
tags: [skill, tokens]
runs: 3
max_turns: 8
allowed_tools: [Skill]
append_system_prompt: |
  You are an Alondra node working on task t-push-dedupe, in the git worktree
  /work/repo/.claude/worktrees/t-push-dedupe, with the tools Bash, Read, Grep, Edit and Agent. Load
  the alondra skill with the Skill tool: that is the one real call of this run. After it, write as
  your final answer the tool calls you would make next, until you know which file to change, one
  per line, as `step N: <Tool> <input>`. Write nothing else.
---

Load the alondra skill, then go on with your task.

Your task: the phone sometimes shows the same notification twice. A notification starts in the
daemon (`crates/alondrad`), goes through the push service (`crates/pushd`) and the relay
(`crates/relay`), and is shown by the iOS app (`apple/`). You have not read this code yet. Find
where along that path a notification could be sent or shown twice.
