---
name: alondra
description: How a node works on its Alondra task — asking the user, delivering the result, registering a PR, merging one the user authorized, or saying it waits for the machine. Use it when you start on an Alondra task, need a decision, or are done.
---

# Alondra

The user supervises the fleet; the Alondra MCP instructions give their name and the language to
write to them in. Your terminal does not reach the user: only Alondra's tools do, and the user's
answers are typed into this session. Alondra sees on its own that you are working.

## Your task

The work you were launched for is an Alondra task; its id (`t-…`) is in your mission. Pass it as
`task_id` (or in `scope`) on every call about it. A node works on one task at a time: work only
on the task assigned to you. Alondra shows the task as working from your first step; `start_task`
is only needed for a task the orchestrator hands you after your previous one ended.

Each call needs a new unique `interaction_id` (or `pr_id`, `document_id`, `revision_id`) that you
choose, for example `t-12-result-1`. When a call fails, nothing was recorded and the reply says
why; retry an uncertain call with exactly the same arguments.

## The tools

- `ask`: a decision for the user (`recipient` kind `user`), or a real question for the
  orchestrator (kind `orchestrator`). One question per decision, all the context in `body`, and
  options whose `description` says what choosing each means. Alondra types the answer into this
  session, so end your turn after asking. A question left in your last message is never seen.
- `request_action`: something the user does outside Alondra (test on a device, get a key, check
  a console), with the steps in order. Their answer is typed into this session.
- In a background session, menu questions (`AskUserQuestion`), permission prompts and plans
  also reach the user in Alondra, and their answer comes back as Claude's own.
- `publish_information`: a finding the user (or the orchestrator) should read now. It changes
  nothing for the task.
- `present_result` with `is_final: true`: you are done. The answer goes in `body`; `targets` names
  what you deliver: `text`, a document revision you created, or the PR you registered with its
  exact head. The user accepts it in Alondra. A file sent another way or a final message left in
  this session is not a delivery.
- When the user accepts your final result, Alondra closes the task itself (for a PR, once its
  merge is confirmed); there is nothing left to call.
- `complete_task`: only under auto-merge, after you merged the PR yourself, with the final
  presentation's id.
- `create_document` / `revise_document`: a file the user reviews. Copy it into Alondra's files
  folder (`$ALONDRA_HOME/files/`) and pass that path with its
  SHA256; each revision is reviewed as it is. Creating it shows the user nothing by itself.
- A plan (the user asks you to plan, or the work needs a plan approved first): enter plan mode
  with `EnterPlanMode`, plan, and present it with `ExitPlanMode`. Alondra holds it as a plan card:
  the user reads the whole plan, comments on any block, and approves or requests changes; the
  requested changes reach you as a message right after. Never write a plan as a document or ask
  about it with `ask`.
- Another document the user approves before you go on (a design, a report, a spec): create (or
  revise) it, `ask` with the decision in `body` and the approval options, then at once
  `link_interaction_subject` with that question and
  `{kind: "document_review", document_id, revision_id}`. The user gets the review card: the whole
  document, a comment on any block, your options and Request changes, whose comments come back as
  the answer. Never ask a separate plain question about a document, and do not paste the document
  into `body`.
- `withdraw_question`: a question of yours that no longer needs an answer.
- `wait_for_machine`: you are stopped by RAM, a build queue or a usage limit.

## What a delivery is not

- Progress. While you work on your own, including waiting for a build, an upload or a review,
  there is nothing to send.
- A question. Anything the user has to decide, approve, test or fetch is an `ask` or a
  `request_action`, never buried in a result or an information message.

## Your PR

- Right after opening a PR, call `register_pr` with the task, its host, `owner/repo`, number,
  head and base, and the merge methods allowed. One task has one PR for its whole life; other
  work needs another task.
- Present it with `present_result`, the PR and its head in `targets`. The user decides the merge
  on its card, so do not ask about merging. When they authorize it, Alondra hands you the
  authorization: check the PR's head and base, call `claim_merge_authorization`, and only then
  run the approved method against that PR and head, for example
  `gh pr merge <number> --match-head-commit <head> --squash`. Never merge without that claim.
- With auto-merge chosen for the work, register and present the PR the same way and merge it as
  your mission says once its checks pass.
- A task that builds on another names the branch to start from and to open your PR against.

## One task at a time

Your subagents work on your task; they need no task of their own. Something new the user asks
for that is not part of your task goes to the orchestrator (`ask` with kind `orchestrator`): it
launches another node for it.

## Probes and end-to-end tests

Probes and end-to-end tests run against a sandbox, never the user's live Alondra: anything they
send lands in the user's real inbox. `make e2e-sandbox` in the Alondra repo starts a throwaway
daemon and prints the `claude --bg --name test/<probe> …` line for a probe node. Name probes
`test/<something>` and remove them and their worktrees when done.
