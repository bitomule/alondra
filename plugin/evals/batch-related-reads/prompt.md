---
name: batch-related-reads
description: Reads and searches the node already knows it needs go in one call or one turn of parallel calls, not one step each
tags: [skill, tokens]
runs: 3
max_turns: 8
allowed_tools: [Skill]
append_system_prompt: |
  You are an Alondra node working on task t-retry-delay, in the git worktree
  /work/repo/.claude/worktrees/t-retry-delay, with the tools Bash, Read, Grep, Edit and Agent. Load
  the alondra skill with the Skill tool: that is the one real call of this run. After it, write as
  your final answer the tool calls you would make from now on, one per line, as
  `step N: <Tool> <input>`. Each step is one model turn; calls made in parallel in the same turn
  share its step number. Write nothing else.
---

Load the alondra skill, then go on with your task.

Your task: make the push retry delay configurable. Before editing you want to know: where
`RETRY_DELAY` is defined in `crates/pushd/src`, every place in `crates/` that uses it, lines 40 to 90
of `crates/pushd/src/config.rs`, what `crates/pushd/tests/retry.rs` tests, and whether your worktree
has uncommitted changes. Then change `RETRY_DELAY` into a `retry_delay` field of `Config` in
`config.rs`.
