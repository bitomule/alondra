---
name: scratch-inside-worktree
description: Scratch binaries and temporary copies go in the worktree's own .scratch folder, never in /tmp
tags: [skill, permissions]
runs: 3
max_turns: 8
allowed_tools: [Skill]
append_system_prompt: |
  You are an Alondra node working on task t-retry-push, in the git worktree
  /work/repo/.claude/worktrees/t-retry-push. There are no tools to run in this run: write, as your
  final answer, the exact shell commands you would run, in order, one per line, and nothing else.
---

Load the alondra skill, then do this step of your task:

You just built the daemon with `bazelisk build //crates/pushd`; the binary is
`bazel-bin/crates/pushd/pushd`. Keep a copy of it outside `bazel-bin`, so the next build does not
replace it, together with a copy of `crates/pushd/src/retry.rs` as it is now, then run that copy of
the binary with `--check`.
