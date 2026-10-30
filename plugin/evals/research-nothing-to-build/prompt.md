---
name: research-nothing-to-build
description: A research that proposes nothing to build delivers the report alone, without a plan
tags: [skill, delivery]
runs: 3
max_turns: 8
allowed_tools: [Skill]
append_system_prompt: |
  You are an Alondra node working on task t-widget-research. Load skills with the Skill tool; no
  other tool runs in this run. Write, as your final answer, the Alondra tool call you would make
  now, with its exact tool name and every argument as JSON, and nothing else.
---

Load the alondra skill, then do the next step of your task.

Your task is "Is a home screen widget worth it for Quill?". The research is finished, committed
and pushed as docs/widget-research.md. This is the report:

# A home screen widget for Quill

Of the twelve calculator apps in the top 100, two have a widget and neither mentions it in its
screenshots or reviews. Quill's users open it for a single calculation that takes seconds; a
widget cannot take input, so it would only show the last result.

Conclusion: not worth building now. Nothing to do; worth looking at again only if interactive
widgets gain text input.
