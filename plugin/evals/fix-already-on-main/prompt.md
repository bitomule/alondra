---
name: fix-already-on-main
description: A fix task whose problem no longer happens on a fresh main ends as already fixed, with its evidence and no card for the user
tags: [skill, delivery]
runs: 3
max_turns: 8
allowed_tools: [Skill]
append_system_prompt: |
  You are an Alondra node working on task t-card-fusionar-copy, "La tarjeta de un PR dice «Fusionar»
  en español", in the git worktree /work/alondra/.claude/worktrees/t-card-fusionar-copy. First load
  the alondra skill with the Skill tool: that is the one real call of this run. Alondra's tools are
  not available here, so after it write, as your final answer, the Alondra tool calls you would
  make, in order, one per line, each with its exact tool name and every argument as JSON.
  Alondra's instructions for this session: the user is David; write user-facing content in Spanish,
  and your replies in this terminal too: the apps show your last one beside your work.
---

Load the alondra skill with the Skill tool, then do the next step of your task.

Your mission: "main fails views::tests::spanish_says_merge_for_a_pull_request on origin/main
ad337ea4: the Spanish pull request card says «Merge» where the test expects «Fusionar». Fix it."

So far you read crates/core/src/views/cards.rs, changed the merge label to `tr!("Merge")`, committed,
pushed and opened PR #1161 "The PR card says «Fusionar» in Spanish". Before registering it you
rebased on origin/main and found this:

```
$ git fetch origin main
$ git rebase origin/main
dropping 7f3e2b1c4 The PR card says «Fusionar» in Spanish -- patch contents already upstream
Successfully rebased and updated refs/heads/worktree-t-card-fusionar-copy.
$ git diff origin/main --stat
$ bazelisk test //crates/core:core_test --test_filter=views::tests::spanish_says_merge_for_a_pull_request
//crates/core:core_test                                                  PASSED in 3.1s
Executed 1 out of 1 test: 1 test passes.
$ git log --oneline ad337ea4..origin/main -- crates/core/src/views/cards.rs
4e1c2d0a7 The PR card says «Fusionar» in Spanish again (#1159)
```

PR #1161 is still open and not registered with Alondra.
