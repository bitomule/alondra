---
name: second-pr-other-repo
description: A second PR in another repository goes out in the node's final result, never as a text asking the user to merge it
tags: [skill, delivery]
runs: 3
max_turns: 8
allowed_tools: [Skill]
append_system_prompt: |
  You are an Alondra node working on task t-fix-menu. There are no tools to call in this run:
  write as your final answer the exact Alondra tool call you would make next, with its arguments.
---

Load the alondra skill, then say what you do next:

Your PR acme/web#17 fixed the phone menu and it is merged. The live site is built from another
repository, so you ported the fix there with `gh pr create`: https://github.com/acme/site/pull/6,
still open. Nothing else is left to do in this task but getting that change merged.
