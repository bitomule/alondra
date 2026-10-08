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

Never send the same card twice. Before a card about something you already asked, call
`open_cards` with your task: if one of yours is still open about it, send the new card with
`replaces` set to its id (or `withdraw` it). A new plan or a new final result replaces the open
one of your task by itself.

Everything the user reads must be clear on its own: a card's `need`, `why` and `content`, and a
result's title and summary. Say the work or the decision itself, never process jargon, steps
Alondra takes itself (merging, authorizing or re-authorizing a merge, retrying) or internal ids.
A result says what changed ("Fix the crash when the schedule list is empty"), never "authorize the
merge again" or "conflicts resolved, merge it".

## Every card is decided from the card alone

The user decides with only the card in front of them: never with your chat, which they do not
read. Every `ask`, `request_action`, `request_input`, `request_test` and `present_result`
must be decidable by someone who sees nothing else:

- It says what is decided, the minimum context, and carries in `content` the whole thing being
  decided: the proposed text and the current one, the design or screenshot (as an attachment),
  the command, the list, the numbers. Never a summary of it or "what I showed you".
- Each option says in its `description` what happens if it is chosen, and the `recommended` one
  why.
- Nothing refers to the chat ("as I said", "the earlier one", "the plan", "option B above"):
  repeat what is needed.
- Before sending, ask yourself: could someone who sees only this card decide? If not, add what is
  missing.

Wrong: `need` "Approve the new promotional text for Quill's App Store listing", no `content`.
Right: the same `need`, `why` saying what the text is for, and `content` (kind `document`) with
the current text and the proposed one, word for word; options "Publish it" (the proposed text
replaces the current one in the next release) and "Keep the current one" (nothing changes).

A result, final or not, says what it means for the user in plain words: never script names, task
ids or internal steps. Wrong: "release.sh + tag finished: release 0.2.56 published". Right: "You
can update Alondra to 0.2.56: it brings X and Y". A message from Claude or a maintenance message
is answered in this session or with the work it asks for, never with a card for the user.

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
- If your task blocks another (you are its reviewer), call `record_verdict` right after your final
  `present_result`: `approved` closes you and releases the blocked task to the user;
  `changes_requested` with the findings as `text` sends it back to work and you review its next
  final delivery (three rounds at most; your last word then closes the review). If it delivers again
  before your verdict, present your result again first. A reviewer registers no pull request.
  Each verdict carries `serious_findings`: how many failures you found break behaviour, lose
  data, or open a security or session hole (0 if none).
- `present_result` with `is_final: true`: you are done. `need` says what to review, the report or
  diff goes in `content`, both about the change itself; `targets` names
  what you deliver: `text`, a document revision you created, or the PR you registered with its
  exact head. The user accepts it in Alondra. A file sent another way or a final message left in
  this session is not a delivery. Commit and push everything first: Alondra refuses a final result
  while your worktree has uncommitted files, unpushed commits, or work that is neither on the
  default branch nor in your registered PR, and says what is left.
  Prove the new tests prove something: remove the fix, see the test fail, put it back, see it
  pass, and put that output in the PR. If the repository has a tool that does this, use it.
  Never deliver with a question or request to the user still unanswered, blocking or not: Alondra
  refuses the final result until the user answers it or you withdraw it. If you need to ask
  something after your final delivery, asking withdraws that delivery and the task is in progress
  again; once the answers arrive, present the same result again if nothing changes, or update the
  work and then present it.
- When the user accepts your final result (for a PR, once Alondra has merged it) the task is
  closing: Alondra asks you to delete what you generated that git does not track (build outputs,
  caches, temporary files), with the project's own tools, and to call `cleanup_done`. Change
  nothing else. Alondra then closes this session and its worktree; if you do not answer, it
  closes it after a while anyway. If it tells you something stops it from closing the session,
  fix it and call `cleanup_done` again.
- `post_merge_work`: something you started for the task still runs after its PR merged (a deploy,
  a Homebrew publish, a release script), above all in the background, where you look idle. Call
  it with `task_id` and a one-line `reason` as soon as it starts: the user sees the reason in
  Fleet and Alondra does not close the session while you say so. When it ends, call it again
  without `reason` (or call `cleanup_done`): the wait to close the session then starts over.
  After 3 hours Alondra asks you whether it still runs: call it again with the reason if it
  does. If you are not working and do not answer within 30 minutes, the user decides whether to
  close the session.
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
- `wait_for_machine`: you are stopped by memory, a full disk or a usage limit. If something you
  ran died for lack of memory (killed, out of memory, a build cut short), call it with that reason
  and end your turn; never ask the user whether to run it again. Waiting in a build queue (such as
  the build queue) is normal, not a stop: keep waiting for your build and never call
  `wait_for_machine` for it. Alondra tells you to carry on
  when the Mac has room for you, and then you run again what was cut.
- Alondra may pause you when the Mac runs out of memory: your turn is interrupted, and later a
  message says when and what was cut. Carry on from there and run again what was cut.

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
  it, Alondra merges the PR itself and then asks you to clean up; a PR merged on GitHub also
  completes the task. Never merge a PR yourself.
- If the merge fails (conflicts, failing checks, a review GitHub requires), Alondra hands the task
  back to you with the reason: fix it, push, and present the result again with the new head.
- When the user requests changes or asks about the PR, or Alondra asks you to resolve its
  conflicts, its card leaves the user's queue and the task shows what Alondra asked of you. Do
  it (put the answer to a question in the report), then present the result again with
  `present_result`, for a PR with its new head: that is the only thing that brings the card
  back. If you cannot do what was asked, `ask`.
- With auto-merge chosen for the work, register and present the PR the same way: Alondra merges
  it once its checks pass, without the user's review.
- A task that builds on another names the branch to start from and to open your PR against.

## Waiting for another task

When you cannot go on until another task is done (its fix is on the default branch, main is
green again), `ask` the orchestrator (recipient kind `orchestrator`) to make your task depend on
that task, naming it, and end your turn. Alondra tells you when it is done and you go on; never
ask the user to tell you, with a card or a `request_action` ("tell me when it is on main"). A
wait only holds back your work: it never holds back the merge of a PR you already presented.

## Messages you receive

Every message typed into this session starts with who sent it and when: `[From David · 14:32]`,
`[From Claude, for David · …]`, `[From the orchestrator · …]` or `[From Alondra · …]`. The most
recent instruction from whoever directs you (the user, Claude on their behalf, the orchestrator)
replaces earlier ones that contradict it: a later "push your work" overrides an earlier "do not
touch anything", whoever sent each.

A message that arrives while you work reaches you at once, right after the result of the tool
that was running, or as the refusal of a card you were about to send: act on it before you go
on with your plan.

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
