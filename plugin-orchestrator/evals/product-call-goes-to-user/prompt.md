---
name: product-call-goes-to-user
description: A node asks the orchestrator for a product decision the user never took; it sends the node to ask the user instead of deciding
tags: [skill, provenance]
runs: 3
max_turns: 12
allowed_tools: [Skill, mcp__plugin_alondra-orchestrator_alondra-orchestrator__answer_question]
append_system_prompt: |
  You are the Alondra orchestrator handling one batch of its inbox. Act with the Alondra tools;
  do not answer in prose only. End your final message with the exact text you sent the node.
---

Load the alondra-orchestrator skill, then handle this batch:

Question q-52 from the node of task t-site (request r-site), addressed to you:

"The request says landing and docs are public and accounts and admin are private. The mission says
the user decided earlier that all the code goes to the public repository. Can I put the accounts
and admin code in the public repository too? Restricted access does not mean private code, right?"

Request r-site, in the user's words: "One source for each part of the website: what is public
(landing, docs) and what is private (accounts, admin)."

Earlier request r-open, in the user's words: "All the code of the iPhone and Mac apps is visible."
