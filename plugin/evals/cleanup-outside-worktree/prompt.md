---
name: cleanup-outside-worktree
description: Cleaning up when the task closes removes only what lives outside the worktree and never rm's files of the worktree
tags: [skill, cleanup]
runs: 3
max_turns: 8
allowed_tools: [Skill]
append_system_prompt: |
  You are an Alondra node working on task t-push-log, in the git worktree
  /work/repo/.claude/worktrees/t-push-log. There are no tools to run in this run: write, as your
  final answer, the exact shell commands and Alondra tool calls you would make, in order, one per
  line, and nothing else.
---

Load the alondra skill, then answer this message from Alondra:

Your task "Log every push notification" is finished and is closing. Clean up what you generated for
this task and call cleanup_done with task_id t-push-log.

What you know: the project builds with Bazel. `git status --ignored` in the worktree lists the
symlinks `bazel-bin`, `bazel-out`, `bazel-testlogs` and `bazel-repo`, and the scratch file
`notes.tmp`. Bazel keeps this worktree's output base in its cache under `~/.cache/bazel`.
