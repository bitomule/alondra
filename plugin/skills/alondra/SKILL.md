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
install`, `brew`, `launchctl`, `alondra doctor --fix` or `alondra update`. Test with the e2e sandbox
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
  Every option is a button: pressing it answers. The user can always write on the card too,
  alone or with the option they press, so never add an «Other: write it» option; to ask for a
  value (a name, a number, a key), use `request_input`.
  Ask related questions together: send `questions` (2 to 4, each `{question, options, recommended,
  why}`) instead of `options`. Each question becomes its own card, the user answers them one by
  one, and every answer comes back together, each to its own question, once the last is answered;
  you never get part of them. Every question must stand on its own and be clear without any
  context from your work, because the user sees one card at a time. Never write "(Recommended)" in
  a label: `recommended` is the mark. A question's `why` and the card's `content` are only context
  for answering (why each option matters), never more questions or options.
- `request_input`: a file, text or secret only the user has (`kind`: file, text or secret). The
  user writes or attaches it on the card and it comes back as their answer.
- `request_action`: something the user does outside Alondra (get a key, check a console), with
  the `steps` in order and `verify`, how you will check it was done. Their answer is typed into
  this session.
- `request_test`: the user tries a build or screen: `where` (build, version, screen) and `what`
  to look at. They answer Works or Doesn't work, with text.
- `suggest`: something you saw outside your task (an unrelated bug, an improvement). Send it as
  soon as you see it, with `what` you saw, `where` and `why_not_mine`, and keep working: it goes
  to the orchestrator, never to the user, and never blocks you or your delivery. Never put it in
  your result or ask the user about it. The orchestrator answers with one decision: part of your
  plan (do it in this task), discarded (with the reason), or a new task it already created (with
  its id). At most five wait per task.
- In a background session, menu questions (`AskUserQuestion`), permission prompts and plans
  also reach the user in Alondra, and their answer comes back as Claude's own.
- Run a command that may need the user's approval on its own, not chained with `;`, `&&` or a
  pipe to other commands, and run read-only checks (`git status`, `tail`, `ls`) as separate
  calls. Never chain a delete or a `reset --hard` with anything else.
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
  When the work found nothing for the user to look at (a scheduled review with no open PRs or
  issues, a check where nothing changed), deliver with `nothing_to_review: true` and only `text`
  targets: Alondra creates no card and closes the task in silence. Never send a review card that
  only says there was nothing to do.
  A report that ends in work to build (a research that proposes how to do it) sends that work as
  `plan`, Markdown with one checklist line (`- [ ] …`) per thing to deliver, never only inside the
  report. Its card offers "Approve and start implementing", which creates a new task that
  implements exactly that plan, and "Only archive". Alondra keeps every final report (and its
  plan) in its files folder and shows on the card each Markdown file it cites: write the report
  in `content` and never leave it only in a temporary folder. A plan that does not fit one PR
  goes in phases, as "A plan in phases" says.
  When the user approved a plan for the task, send `checklist` with the final result: every point
  of that plan's checklist left to this task, by its number from 1, each with at least one piece of evidence
  (`commit` sha in your branch, `test` or `command` you ran, `screenshot` attachment id, `pr`,
  `file`, `link`). Alondra refuses the result while a point is missing and names it. A point that
  no longer applies goes out with a revised plan the user approves, never by leaving it out.
  When the result needs the user to choose anything (a design's open decisions, a default to
  pick), ask it with `ask` before you deliver: each choice a question with the option you
  recommend, up to 4 together with `questions`. Put the answers into the work, then deliver.
  Never leave choices only in the result's content: accepting a result never answers them.
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
- Work that comes after the merge of your PR (publish, install, deploy, verify) is part of your
  task: the PR alone is not its final result. Declare it with `post_merge_work` and its `reason`
  before you present the PR as final. Accepting and merging the PR then does not close the task:
  it stays in progress, Alondra tells you to do that work once the PR merges, and the task closes
  only after you call `post_merge_work` without `reason` (or `cleanup_done`) when it is done.
- Do not end your turn while your task is open with no result and no question: Alondra sends you
  back to work with the reason, and after three times it tells the user you cannot deliver.
- `create_document` / `revise_document`: a file the user reviews. Copy it into Alondra's files
  folder (`$ALONDRA_HOME/files/`) and pass that path with its
  SHA256; each revision is reviewed as it is. Creating it shows the user nothing by itself.
- Tell understanding from planning. "Help me understand X", "how do others do it", "is it
  viable" asks for a report: deliver it with `present_result`. "Give me a plan for X", "a detailed
  plan to implement it" asks for a plan, also when it comes as an answer to a report you
  delivered: it goes through plan mode, never as a new version of the report, and approving it
  starts the implementation in this task.
- A plan (the user asks you to plan, or the work needs a plan approved first): enter plan mode
  with `EnterPlanMode`, plan, and present it with `ExitPlanMode`. Alondra holds it as a plan card:
  the user reads the whole plan, comments on any block, and approves or requests changes; the
  requested changes reach you as a message right after. Never write a plan as a document or ask
  about it with `ask`. Write its scope as Markdown checklist lines (`- [ ] …`), one per thing you
  will deliver: a plan without them comes back to you, and once approved they are what your final
  result must mark done. The plan goes whole in the card, never as a path: the user cannot open a
  file from the app, so a Markdown file the plan cites (`docs/plan.md`) is added at its end from
  your worktree, and a plan citing one Alondra cannot read comes back to you.
- A plan in phases. Split your plan into phases when its change does not fit one PR a person
  reviews in one sitting. It does not fit when any of these is true: it changes more than one
  layer that ships on its own (stored data or its schema, the service, its API, each client
  app); it deletes or renames something others still read (a route, a table, a field, a public
  command); it changes a server and its clients at once; or it adds a mechanism and also moves
  every caller onto it. Each phase is one PR, written as a heading with its checklist lines
  under it, and says three things: what it delivers that works on its own (merged alone,
  nothing breaks); how it is measured or checked (the test, number or capture that proves it);
  and what it truly depends on, by phase number: only the phases whose code it needs, "none"
  when it needs none, never "the one before" by habit. Say when a phase's PR starts from
  another phase's branch instead of the default branch (it stacks on it). Phases that need the
  same earlier phase and not each other depend on that phase alone, so they run in parallel.
  Put first a phase that depends on none: that is the one you build in this task.
- When the user approves a plan in phases, `ask` the orchestrator (recipient kind
  `orchestrator`) to create one task per other phase, listing for each its title, the numbers
  of its checklist points, the phases it depends on and whether it stacks on one; then end your
  turn. Those points leave your checklist for their tasks: your final result marks only the
  points left to you, in your one PR.
- Another document the user approves before you go on (a design, a report, a spec): create (or
  revise) it, `ask` with the decision in `body` and the approval options, then at once
  `link_interaction_subject` with that question and
  `{kind: "document_review", document_id, revision_id}`. The user gets the review card: the whole
  document, a comment on any block, your options and Request changes, whose comments come back as
  the answer. Never ask a separate plain question about a document, and do not paste the document
  into `body`. A document with open decisions asks them first, with `ask` as above, and records
  the answers: the review approves a document with nothing left to choose.
- `withdraw`: a card of yours that no longer needs an answer (`withdraw_question` still works).
- `answer_clarification`: the user asked you to clarify a card. Until you settle it your next card
  must replace it: send `replacement` (the corrected card, as `{tool, arguments}`) with the
  explanation in its `why` or `content`, or `withdraw` it.
- There is no tool to authorize anything. Permission prompts reach the user from Claude Code.
- `wait_for_machine`: you are stopped by the Mac's memory, a full disk or the CPU. The
  subscription's usage (weekly or 5-hour limits, tokens, a usage warning) is never a reason to
  wait or stop: Alondra paces usage itself and the user decides whether to keep going, so keep
  working; Alondra refuses a wait for usage. If something you
  ran died for lack of memory (killed, out of memory, a build cut short), call it with that reason
  and end your turn; never ask the user whether to run it again. Waiting in a build queue (such as
  the build queue) is normal, not a stop: keep waiting for your build and never call
  `wait_for_machine` for it. Alondra tells you to carry on
  when the Mac has room for you, and then you run again what was cut.
- Alondra may pause you when the Mac runs out of memory: your turn is interrupted, and later a
  message says when and what was cut. Carry on from there and run again what was cut.
- Never stop on your own for the subscription's usage, and never ask the user or the orchestrator
  whether to stop or wait for it: only the user (on the usage card) and Alondra (pacing the fleet)
  decide that, and Alondra refuses such a question. If Alondra pauses you for usage, Alondra
  resumes you.

## What a delivery is not

- Progress. While you work on your own, including waiting for a build, an upload or a review,
  there is nothing to send.
- A question. Anything the user has to decide, approve, test or fetch is an `ask`,
  `request_action`, `request_test` or `request_input`, never buried in a result or an
  information message.

## Your PR

- Right after opening a PR, call `register_pr` with the task, a short `pr_id` such as `pr-123`
  (letters, digits, `-` and `_` only, never `owner/repo#123`), its host, `owner/repo`, number,
  head (the full 40-character commit sha you pushed, `git rev-parse HEAD`, never a branch name)
  and base, and the merge methods allowed. A task owns exactly one PR for its whole life:
  more work for the task goes on that PR, and work that needs another PR is another task, which
  you ask the orchestrator for (`ask` with recipient kind `orchestrator`); a plan in phases asks
  for one per phase but yours. A second `register_pr`
  for the task is refused.
- Present it with `present_result`, the PR in `targets`, as your final result: leave its `head`
  out and Alondra uses the commit the PR is at, or give that full commit sha. The
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
