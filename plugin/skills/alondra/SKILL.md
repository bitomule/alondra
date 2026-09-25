---
name: alondra
description: How to talk to the user and to the orchestrator through Alondra. Use it when you need a decision from the user, when you deliver something, or when you are stopped for lack of machine resources.
---

# Alondra

The user is the person who supervises the fleet; the Alondra MCP instructions give their name
and the language to write to them in. Alondra sees your state on its own. Your terminal does
not reach the user: only Alondra's tools do, and the user's answers are typed into your session.

## The tools

- `ask`: a decision for the user. One question per decision, a one-line title (the
  notification shows it), all the context in `detail`, options, PRs in `links` and files in
  `attachments`. Alondra types the answer into this session, so end your turn after asking. A
  question left in your last message is not seen. With `to_orchestrator: true` the same
  question goes to the orchestrator instead of the user — use this only for a real decision
  that needs an answer, arriving through the fleet digest (up to ~90s), never to hand off work.
- `request_action`: something the user does outside Alondra (test on the iPhone, get a key,
  check App Store Connect), as a title, the steps in order and the links. The user answers
  "Done" or says why they cannot, and that is typed into this session.
- Menu questions (`AskUserQuestion`) also reach the user, for short choices.
- `report` with `kind: "final"`: you are done and nothing is pending — the answer, with the file
  in `attachments`. The usual way to end.
- `report` with `kind: "notice"`: a result or finding the user should know now, with the result
  in the title; nothing changes for the task.
- `report` with `kind: "document"`: only for an artifact you publish for the user to review, its
  claude.ai URL in `links` (see "Your PR" below). A document never moves your task: if you are
  delivering the answer, send it as `final` with the file in `attachments`; if the user has to
  decide something — including whether to open a task to fix it — send an `ask` with the file
  attached and the options.
- `report` with `to_orchestrator: true`: a handoff, not a result for the user — "someone should
  continue this" (a follow-up, next steps, what you measured that the orchestrator should act
  on). Use `kind: "final"` when you have nothing left of your own pending; you are done and the
  orchestrator picks it up from its own inbox (the fleet digest, up to ~90s). This is not a
  question: never use `ask` for a handoff, even with `to_orchestrator: true`, since that puts a
  decision on the orchestrator you are not actually asking it to make.
- `new_task`: work that is not your task's, found while you do it (a follow-up the PR under
  review caused, a problem in another place). It opens a task of its own in the same request,
  assigned to you, and returns its id; its questions and PR go there (see "Your task").
- `wait_for_machine`: you are stopped by RAM, a build queue or a usage limit.
- `ask` with `replaces`: a question of yours changed (rephrased, or new facts change it). The new
  question is recorded and the old one, given by its number, is withdrawn in the same step, so
  the user never sees both.
- `withdraw`: a question of yours no longer needs an answer (settled in the terminal, its PR
  merged, you moved on). Alondra withdraws a question sent with `about_pr` once that PR is merged
  or closed; a question that only links a PR stays until it is answered.

## Your task

- What you work on is a task in Alondra. Everything you send for it shows as one card: your
  reports go inside its PR card while one is open, and so does a question sent with `about_pr`;
  else your question carries your reports, else your reports stack under the newest document.
- A task puts one question (or action) at a time in front of the user. A second one while the
  first is open is refused: wait for the answer, send it with `replaces` if it changes the open
  one, or `withdraw` the open one first. A permission prompt waiting for the user counts as that
  one question too.
- While a PR of your task waits for review, that PR is the task's one pending point. A question
  or action whose answer changes what the PR contains or how it ships (deploy it now or later,
  test it on the device) goes with `about_pr` set to its number and shows in its card. Anything
  else, even work the PR caused or that you found while doing it, is a separate piece of work:
  open it with `new_task` (a title, and a brief if it helps), then `ask` with `task` set to the
  id it returns. Alondra refuses a question without `about_pr` while the PR waits, and says so.
  From then on you work on several tasks, so name the task on every call.
- If you work on several tasks, say which with `task` on `ask`, `request_action`, `report` and
  `withdraw`; Alondra lists them when it cannot tell. With one task you never need it.
- When Alondra refuses a call, nothing was recorded and the reply says why and what to do.
- Once your task is done (your final report), the user gets no new questions from it: if there is
  more to do, ask the orchestrator (`to_orchestrator: true`) or hand it over with
  `report(kind: final, to_orchestrator: true)`.
- A task that builds on another comes with the branch to start from and to open your PR
  against; follow it, not the default branch. Your PR merges after the one below it.

## Several tasks

The orchestrator can give you another task while you work: Alondra types it in as
"[Alondra] A new task for you, from the orchestrator: t-… “…”", and a message about one of your
tasks starts with "[Task t-… “…”]".
- Work each task in a subagent of its own (Agent tool), named after the task (`name: "t-12"`),
  with the task's brief as its prompt, so the tasks never share a context. Subagents have
  Alondra's tools: the subagent asks, reports and opens its PR itself, passing `task` on every
  `ask`, `request_action`, `report` and `withdraw`.
- A task on a different branch gets its own worktree. A subagent's automatic worktree always
  starts from the default branch, so create it with git when the task names a base
  (`git worktree add .claude/worktrees/t-12 -b <branch> origin/<base>`) and have the subagent
  work there.
- A subagent that asked the user ends its turn. The user's answer, and anything the orchestrator
  sends about a task, arrives in this main session: pass it to that task's subagent with
  `SendMessage` (to: its name or agent ID), which continues it with its context. Do not answer
  for it, and do not relay one task's answer to another task's subagent.
- Each task ends with its own final report, with `task`; the others go on.
- Every PR you or a subagent open for one of them has "Alondra task: t-…" in its body: Alondra
  files it under that task. `task` on a call from this main session counts for that call only.
  A PR Alondra cannot place gets no task, and Alondra types "PR #N: which task is it for?": answer
  with a report that links the PR, with `task`.

## Your PR

- A PR you open (Alondra sees `gh pr create`) gets its own card, where the user merges,
  asks, requests changes or sets it aside. The merge decision lives on that card, so an `ask`
  about merging would be a second card for the same decision.
- If nothing is left for you after the merge, send your final report with the PR in `links`: it
  shows inside the PR card and the merge finishes your task; no notice comes. If something is
  left (deploy, install, check on main), do not report final yet: the merge notice is typed
  into this session, then do what it unblocks and report final.
- When the user asks about the PR or requests changes, answer with a report linking it, or push:
  either puts the PR back in front of the user.
- With auto-merge, Alondra merges the PR itself once its checks pass and GitHub can merge it.
  Open it and end your turn; do not merge it yourself or wait on its checks. Alondra types the
  merge notice into this session, or what only you can fix (failed checks, conflicts).
- An artifact you publish for the user to review goes out as a `report` with kind `document` and
  its URL in `links`. That report is a review card: the user comments on the artifact in the
  browser and then approves it or sends the comments to you from the card. You count as waiting for
  the user's review until then, and a final report does not finish you while it is open. The
  comments arrive in this session as the user's words: read them with `ArtifactComments` (action
  `read`), apply them, republish to the same URL, and send a new `document` report linking it for
  the next round. Once it is approved, a later report about that artifact asks nothing of the user.

## What a report is not

- Progress. Alondra rejects reports that say you are still working, do not need the user yet,
  or list steps under way, and you lose a turn. While you work on your own, including waiting
  for a build, an upload or a review, there is nothing to send.
- A question. A report that asks the user to decide, approve, test or fetch something is
  delivered flagged and the question gets lost in the reading; it belongs in `ask` or
  `request_action`.

## Probes and end-to-end tests

Probes and end-to-end tests run against a sandbox, never the real alondrad on port 47800:
anything they send lands in the user's live inbox (a test once typed a fake «Mergea» into a
real node). Alondra refuses `report`, `ask` and `request_action` from nodes named `test/…`
unless it runs as a sandbox.

`make e2e-sandbox` in the Alondra repo starts a throwaway alondrad with its own data directory
and port, and prints the `claude --bg --name test/<probe> --plugin-dir … --settings …` line
that points a probe node at it. Name probes `test/<something>`, close them with `claude rm`
and stop the sandbox when done.
