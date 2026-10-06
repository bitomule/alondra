---
name: alondra
description: How a node works on its Alondra task — asking the user, delivering the result, registering its one PR (Alondra merges it), or saying it waits for the machine. Use it when you start on an Alondra task, need a decision, or are done.
---

# Alondra

The user supervises the fleet; the Alondra MCP instructions give their name and the language to
write to them in. Your terminal does not reach the user: only Alondra's tools do, and the user's
answers are typed into this session. Alondra sees on its own that you are working.

## Never deploy

Never install, restart or replace the Alondra daemon or CLI on the user's machine: no `make
install`, `brew`, `launchctl`, `alondra setup` or `alondra update`. Test with the e2e sandbox
(`make e2e-sandbox`). Deployment is the coordinator's job.

## Your task

The work you were launched for is an Alondra task; its id (`t-…`) is in your mission. Pass it as
`task_id` (or in `scope`) on every call about it. A node works on one task at a time: work only
on the task assigned to you. Alondra shows the task as working from your first step; `start_task`
is only needed for a task the orchestrator hands you after your previous one ended.

Each call needs a new unique `interaction_id` (or `pr_id`, `document_id`, `revision_id`) that you
choose, for example `t-12-result-1`. A retry with the same `interaction_id` never duplicates the
card. When a call fails, nothing was recorded and the reply says why. If it says Alondra could
not be reached, do not proceed: retry the same call. If Alondra refused it, the reply names the
field and how to fix it: fix it and send it again. A refusal is never an answer or a permission,
and sending the same refused card three times raises a Problem for the user instead.

## Cards

Every card the user reads has a `need`, what you need from them: imperative, one line, at most 90
characters. Cards that ask for something also have a `why`: one or two sentences, at most 240,
about why and what it holds up. Name tasks and PRs by their title or number, never by an internal
id such as `t-foo`: the user reads `why` on the card. Anything longer goes in `content` (`kind`, optional `summary`,
`body`), never in `why`: the user sees `need` and `why` on the card and `content` in its detail.
Prefer a recommendation whenever you can make one.

Everything the user reads must be clear on its own: a card's `need`, `why` and `content`, and a
result's title and summary. Say the work or the decision itself, never process jargon, steps
Alondra takes itself (merging, authorizing or re-authorizing a merge, retrying) or internal ids.
A result says what changed ("Fix the crash when the schedule list is empty"), never "authorize the
merge again" or "conflicts resolved, merge it".

Pick the tool by what you need:

## The tools

- `ask`: a decision for the user (`recipient` kind `user`), or a real question for the
  orchestrator (kind `orchestrator`). One decision per card, 2 to 8 options whose `description`
  says what choosing each means, `recommended` set to the option you would pick, the context in
  `content`. Alondra types the answer into this session, so end your turn after asking. A question
  left in your last message is never seen. Only one blocking ask is open at a time: change it
  with `replaces`, or `withdraw` it.
  Ask related questions together: send `questions` (2 to 4, each `{question, options, recommended,
  why}`) instead of `options`. Each question becomes its own card, the user answers them one by
  one, and every answer comes back together, each to its own question, once the last is answered;
  you never get part of them. Every question must stand on its own and be clear without any
  context from your work, because the user sees one card at a time. Never write "(Recommended)" in
  a label: `recommended` is the mark. A question's `why` and the card's `content` are only context
  for answering (why each option matters), never more questions or options.
- `request_input`: a file, text or secret only the user has (`kind`: file, text or secret).
- `request_action`: something the user does outside Alondra (get a key, check a console), with
  the `steps` in order and `verify`, how you will check it was done. Their answer is typed into
  this session.
- `request_test`: the user tries a build or screen: `where` (build, version, screen) and `what`
  to look at. They answer Works or Doesn't work, with text.
- In a background session, menu questions (`AskUserQuestion`), permission prompts and plans
  also reach the user in Alondra, and their answer comes back as Claude's own.
- Nodes send no notices: there is no tool for status, findings or "I am waiting". To decide use
  `ask`, to need an action `request_action`, for input
  `request_input`, to have the user try something `request_test`; report results in
  `present_result`.
- `present_result` with `is_final: true`: you are done. `need` says what to review, the report or
  diff goes in `content`, both about the change itself; `targets` names
  what you deliver: `text`, a document revision you created, or the PR you registered with its
  exact head. The user accepts it in Alondra. A file sent another way or a final message left in
  this session is not a delivery. Commit and push everything first: Alondra refuses a final result
  while your worktree has uncommitted files, unpushed commits, or work that is neither on the
  default branch nor in your registered PR, and says what is left.
- When the user accepts your final result (for a PR, once Alondra has merged it) the task is
  closing: Alondra asks you to delete what you generated that git does not track (build outputs,
  caches, temporary files), with the project's own tools, and to call `cleanup_done`. Change
  nothing else. Alondra then closes this session and its worktree; if you do not answer, it
  closes it after a while anyway. If it tells you something stops it from closing the session,
  fix it and call `cleanup_done` again.
- Do not end your turn while your task is open with no result and no question: Alondra sends you
  back to work with the reason, and after three times it tells the user you cannot deliver.
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
- `withdraw`: a card of yours that no longer needs an answer (`withdraw_question` still works).
- `answer_clarification`: the user asked you to clarify a card. Until you settle it your next card
  must replace it: send `replacement` (the corrected card, as `{tool, arguments}`) with the
  explanation in its `why` or `content`, or `withdraw` it.
- There is no tool to authorize anything. Permission prompts reach the user from Claude Code.
- `wait_for_machine`: you are stopped by RAM, a build queue or a usage limit.

## What a delivery is not

- Progress. While you work on your own, including waiting for a build, an upload or a review,
  there is nothing to send.
- A question. Anything the user has to decide, approve, test or fetch is an `ask`,
  `request_action`, `request_test` or `request_input`, never buried in a result or an
  information message.

## Your PR

- Right after opening a PR, call `register_pr` with the task, a short `pr_id` such as `pr-123`
  (letters, digits, `-` and `_` only, never `owner/repo#123`), its host, `owner/repo`, number,
  head and base, and the merge methods allowed. A task owns exactly one PR for its whole life:
  more work for the task goes on that PR, and work that needs another PR is another task, which
  you ask the orchestrator for (`ask` with recipient kind `orchestrator`). A second `register_pr`
  for the task is refused.
- Present it with `present_result`, the PR and its head in `targets`, as your final result. The
  user decides on its card, so never ask about merging or to authorize a merge. When they accept
  it, Alondra merges the PR itself and tells you ("Pull request #N is merged"); a PR merged on
  GitHub also completes the task. Never merge a PR yourself.
- If the merge fails (conflicts, failing checks, a review GitHub requires), Alondra hands the task
  back to you with the reason: fix it, push, and present the result again with the new head.
- When the user requests changes or asks about the PR, or Alondra asks you to resolve its
  conflicts, its card leaves the user's queue and the task shows what Alondra asked of you. Do
  it (put the answer to a question in the report), then present the result again with
  `present_result`, for a PR with its new head: that is the only thing that brings the card
  back. If you cannot do what was asked, `ask`.
- With auto-merge chosen for the work, register and present the PR the same way: Alondra merges
  it once its checks pass, without the user's review, and tells you.
- A task that builds on another names the branch to start from and to open your PR against.

## One task at a time

You work on one task for your whole life: Alondra closes this session with it. Your subagents work on your task; they need no task of their own. Something new the user asks
for that is not part of your task goes to the orchestrator (`ask` with kind `orchestrator`): it
launches another node for it.

When Alondra tells you your task was cancelled, stop: do not commit, push or deliver anything;
delete what you generated that git does not track, and end your turn.

## Probes and end-to-end tests

Probes and end-to-end tests run against a sandbox, never the user's live Alondra: anything they
send lands in the user's real inbox. `make e2e-sandbox` in the Alondra repo starts a throwaway
daemon and prints the `claude --bg --name test/<probe> …` line for a probe node. Name probes
`test/<something>` and remove them and their worktrees when done.
