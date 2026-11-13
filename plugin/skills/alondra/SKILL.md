---
name: alondra
description: How a node works on its Alondra task — asking the user, delivering the result, delivering its PRs (Alondra merges them), or saying it waits for the machine. Use it when you start on an Alondra task, need a decision, or are done.
---

# Alondra

The user supervises the nodes; the Alondra MCP instructions give their name and the language to
write to them in. Your terminal does not reach the user: only Alondra's tools do, and the user's
answers are typed into this session. Alondra sees on its own that you are working.

## Card fields and title

Every new card has an explicit `title`: its subject, one non-empty line of at most
160 characters. The detail shows it once as the large heading above the content.
`need` is the short action requested, one non-empty line of at most 90 characters.
`why` explains why in at most 240 characters; it is required when the card asks for
something. These are short lines, never the document.

`content.kind` names the content type; `content.body` holds the complete content,
without repeating `title` as its first heading. When content is present, kind and
a non-empty body are required. `content.summary` is an optional label of at most
60 characters. Do not put the title in summary either.

Correct: title "Release design", need "Choose a design", why "The release needs
one design", content {kind: "options", body: "Atlas uses a compact layout; Borealis
uses larger text."}. Incorrect: title "Release design" and body "# Release design"
followed by the same content, or a document pasted into need/why.

For suggest, what supplies the title, why_not_mine supplies why, and where/detail
are content; Alondra supplies need. For close_request, headline supplies the answer
card title and answer supplies content; Alondra supplies need. For an orchestrator
answer_clarification with text, title and need are explicit and text is the content.

This applies to ask, request_input, request_action, request_test,
raise_problem, present_result and every other card producer available to your role.
Each entry in ask.questions has its own title and question (its need), why and options.
For a result, title names what changed and need asks to review it. For a native plan,
the task title provides the card heading: write the plan body without an opening
heading that repeats that title. Start with the proposal or a section heading.

The domain validates structure, required fields, types and lengths, never wording.
It does not compare titles with headings and does not remove or rewrite headings.
A refusal identifies the field to fix. Old stored cards without title remain readable.

## Card language

Every card tool takes a required `language`: `es` or `en`, the language the card is written in.
It must be the user's language, the one the MCP instructions name. Declare what you wrote; the
text is never read, so declaring `es` over an English card does not make it Spanish.
`ask` with `questions` declares it once for all the questions, and `present_result` declares it
on every result, final or not.

When Alondra refuses a card as `card_language_mismatch`, the card was not sent. Rewrite the whole
card in the user's language (title, need, why, content, attachment names) and send it again with
the same `interaction_id` and the `language` the refusal names. Never change only the declared
language: the text must really be in it.


## Never deploy

Never install, restart or replace the Alondra daemon or CLI on the user's machine: no `make
install`, `brew`, `launchctl`, `alondra doctor --fix` or `alondra update`. Test with the e2e sandbox
(`make e2e-sandbox`). Deployment is the coordinator's job.

## Never load the machine

The Mac is shared with every other node. Never create artificial load or occupy its CPU, memory or
disk on purpose: no `yes` processes, spawners, `stress`, busy loops or filling the disk. A problem
that only shows under load is reproduced inside the test, with fake clocks, injected limits and
deterministic fixtures, never by stressing the Mac.

## A public repository

When the repository you work in is public (`gh repo view --json visibility` says PUBLIC, or its
instructions say so), everything you write there can be read by anyone and stays in git history:
commits, pull request titles, descriptions and comments, branch names, code, tests, fixtures and
captures. Never put in them the user's personal data (emails, home paths such as `/Users/<name>`,
ids of their Mac, devices or accounts, public IP addresses), the names of their private apps or
repositories, security problems not yet published, the infrastructure or configuration of a hosted
service, secrets, or the ids of a private tracker. Use synthetic values (`ana@example.com`,
`/Users/ana`, `203.0.113.7`). That context belongs on the cards you send to the user, or in the
private repository the project keeps for it.

Title pull requests and name branches by what changes, link issues as `#N`, and commit with the
user's GitHub `noreply` email. When you find private data already published, tell the user with a
card; never rewrite history, close or delete anything on your own.

## Your task

When your node or a command waits for something, run `alondra report-wait '<what>'`
with optional `--progress '<text>'` and `--detail '<text>'`. Alondra shows these free
texts with their start time, without interpreting them or changing the task's slot.
A script passes `--pid $$` so its report ends with its process; the node omits it.
Use `alondra report-wait --clear` when the wait ends. A report also ends when replaced
or when the node's turn ends. The host supplies `ALONDRA_WAIT_FILE`; without it the
command silently succeeds. The file format is in `docs/wait-report.md`.

The work you were launched for is an Alondra task; its id (`t-…`) is in your mission. Pass it as
`task_id` (or in `scope`) on every call about it. A node works on one task at a time: work only
on the task assigned to you. Alondra shows the task as working from your first step; `start_task`
is only needed for a task the orchestrator hands you after your previous one ended.

Alondra supplies the protocol metadata of every call itself. Your authority over the task comes
from the capability the daemon issued to this session: public session ids and process ancestry
never grant it, and an old session without one must resume with the current plugin.

Each call needs a new unique `interaction_id` (or `document_id`, `revision_id`) that you
choose, for example `t-12-result-1`. A retry with the same `interaction_id` never duplicates the
card. When a call fails, nothing was recorded and the reply says why. If it says Alondra could
not be reached, do not proceed: retry the same call. If Alondra refused it, the reply names the
field and how to fix it: fix it and send it again. A refusal is never an answer or a permission,
and sending the same refused card three times raises a Problem for the user instead.

A tool's arguments are exactly its schema: never add a field it does not have (`title`, `need`, `why` and
`content` go at the top level, there is no `card`), and send an object or a list as JSON itself,
never as text that holds JSON. A call whose arguments do not fit says which ones and what they
must be; nothing was sent, so fix them and call again.

## A fix starts by reproducing the problem

A task that fixes something (a failing test, a red main, a bug someone reported) starts by
confirming the problem still happens on a fresh `origin/main`: fetch it, then run the failing test
the task names, or the cheapest check that shows the bug. Another PR may have fixed it since the
report.

If it no longer happens, there is nothing to fix: do not open a PR, and close any you opened.
Find what fixed it (`git log origin/main` on the files or test involved names the commit and its
PR) and end the task yourself with `present_result`: `is_final` true, `nothing_to_review` true and a
`text` target saying «Already fixed on main by #N», with the evidence: the command you ran, its
output, and the commit and PR that fixed it. Never ask the user whether to close a task that has
nothing left to do.

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
- The `label` of each option names what will be done, not a pronoun for it: "Fix the bug", never
  "Fix it"; "Split into two tasks", never "Do that".
- Nothing refers to the chat ("as I said", "the earlier one", "the plan", "option B above"):
  repeat what is needed.
- Before sending, ask yourself: could someone who sees only this card decide? If not, add what is
  missing.
- The user cannot open a file from the app. `content` holds the card's own text (the decision,
  the report, the answer); a document it goes with (a plan, a design, a spec) is never pasted
  into it. List its Markdown path, relative to your worktree, in the card's `files`
  (`"files": ["docs/design.md"]`): Alondra reads it from your worktree and shows it as a document of
  the card, which the user opens apart, or refuses the card when it cannot read it. A path you only
  mention in the text is never read and never shown. Or send it with `create_document` and, in a
  result, a `document_revision` target.

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
  `ask` is only for a real decision: options that change what you do next. It never announces
  that your work is ready, never shows a report or a plan for the user to read, and never points
  at a file: finished work goes out with `present_result`, and a plan through plan mode.
  Every option is a button: pressing it answers. The user can always write on the card too,
  alone or with the option they press, so never add an «Other: write it» option; to ask for a
  value (a name, a number, a key), use `request_input`.
  Never ask one card after another: gather every question you have open at that point into one
  `ask` with `questions`, and ask again only when an answer raises something new. Never ask what
  you already asked or the user already answered (check `open_cards` and their earlier replies).
  To ask several questions together, send `questions` (2 to 4, each `{title, question, options, recommended,
  why}`) instead of `options`. Each question becomes its own card, the user answers them one by
  one, and every answer comes back together, each to its own question, once the last is answered;
  you never get part of them. Every question must stand on its own and be clear without any
  context from your work, because the user sees one card at a time. Never write "(Recommended)" in
  a label: `recommended` is the mark. A question's `why` and the card's `content` are only context
  for answering (why each option matters), never more questions or options.
- `request_input`: a file, text or secret only the user has (`kind`: file, text or secret). The
  user writes or attaches it on the card and it comes back as their answer. A secret (a key, a
  token, a password) never comes back: give `destination`, a git-ignored file of the repository
  (`.env`) and the variable (`STRIPE_KEY`). Alondra writes `STRIPE_KEY=value` to that file in your
  worktree and in the main checkout, and you only get «STRIPE_KEY written to .env»: read it from
  there. A file git does not ignore, or one outside the repository, is refused.
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
  also reach the user in Alondra, and their answer returns through the active provider.
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
- `present_result` with `is_final: true`: you are done. Finishing is always this call, never an
  `ask`. `need` says what to review, and the whole report, answer or plan goes in
  `content.body`, written out, never a path to the file that holds it: the user cannot open a file
  from the app. Files the user needs go in `attachments`. Both describe the change itself; `targets` names
  what you deliver: `text`, a document revision you created, or your task's PRs. While a PR of your
  task is open, it goes in your final result and nothing else can go out: a text result without it
  and a progress notice (`is_final` false) are refused, because nobody can merge it from there. The user accepts it in Alondra. A file sent another way or a final message left in
  this session is not a delivery. Commit and push everything first: Alondra refuses a final result
  while your worktree has uncommitted files, unpushed commits, or work that is neither on the
  default branch nor in your registered PR, and says what is left.
  When the work found nothing for the user to look at (a scheduled review with no open PRs or
  issues, a check where nothing changed), deliver with `nothing_to_review: true` and only `text`
  targets: Alondra creates no card and closes the task in silence. Never send a review card that
  only says there was nothing to do.
  A research ends in one of two deliveries, never a mix:
  - It proposes nothing to build: the report alone, in `content`, without `plan`. The user reads
    it and marks it Seen; that creates nothing.
  - It concludes that something should be built: first `ask` every decision the plan leaves open
    (each a question with the option you recommend, up to 4 together with `questions`), put the
    answers into the plan, and only then deliver with the report in `content` and the work as
    `plan`: Markdown with one checklist line (`- [ ] …`) per thing to deliver. That result is a
    plan card: the user reads the plan, opens the report as a document beside it, and presses
    Approve (Alondra creates the tasks that build exactly that plan) or Request changes. Its `need`
    asks to approve that plan ("Approve the plan to sync settings with CloudKit"), never to read
    the report or to decide whether to build. The plan carries no options and no open decisions,
    and the report never hides a plan inside it.
  If the user answers a plan card that nothing should be built, deliver again with the report
  alone, without `plan`. Alondra keeps every final report (and its plan) in its files folder and
  shows on the card each Markdown file listed in `files`: write the report in `content` and never leave
  it only in a temporary folder. A plan that does not fit one PR goes in phases, as "A plan in
  phases" says.
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
  closing: Alondra asks you to clean up and call `cleanup_done`. Cleaning up is only what you
  generated outside the worktree, removed with the project's own tool as the project's
  instructions say (for example a build cache kept for this worktree outside it). Never delete files or
  links inside your worktree with `rm`: Alondra deletes the whole worktree right after. If the
  project left nothing outside the worktree, call `cleanup_done` without deleting anything. Change
  nothing else. Alondra then closes this session and its worktree; if you do not answer, it
  closes it after a while anyway. If it tells you something stops it from closing the session,
  fix it and call `cleanup_done` again.
- `post_merge_work`: something you started for the task still runs after its PR merged (a deploy,
  a Homebrew publish, a release script), above all in the background, where you look idle. Call
  it with `task_id` and a one-line `reason` as soon as it starts: Alondra does not close the session while you say so. When it ends, call it again
  without `reason` (or call `cleanup_done`): the wait to close the session then starts over.
  Alondra never asks how long it takes. After the merge, a turn you end with nothing of yours
  running in the background also ends that work, unless you called `post_merge_work` with the
  reason again in that turn or it waits with `waits_for`: declare it again whenever you end a
  turn while the work still runs elsewhere.
- Work that comes after the merge of your PR (publish, install, deploy, verify) is part of your
  task: the PR alone is not its final result. Declare it with `post_merge_work` and its `reason`
  before you present the PR as final. Accepting and merging the PR then does not close the task:
  it stays in progress, Alondra tells you to do that work once the PR merges, and the task closes
  only after you call `post_merge_work` without `reason` (or `cleanup_done`) when it is done.
- When that work, after your PR merged, has to wait for other work to land (another task's fix
  on the default branch), call `post_merge_work` with its `reason` (what you will do then) and
  `waits_for` naming what you wait for: an Alondra task id (`t-…`) or a pull request an Alondra
  task registered (`owner/repo#123`). End your turn: Alondra wakes you with its result and merged
  pull request when it lands. A wait written only in `reason` has nothing that wakes you. If no
  Alondra task delivers what you wait for, `ask` the orchestrator for one, then wait on it.
- Do not end your turn while your task is open with no result, no question, no work of your own
  running and nothing you wait on through an Alondra tool: Alondra wakes you once with the reason,
  and if your next turn ends the same way, the orchestrator decides how your task goes on: it
  tells you what to do, or closes the task.
- `create_document` / `revise_document`: a file the user reviews. Copy it into Alondra's files
  folder (`files/` in Alondra's home, `$ALONDRA_HOME`) and pass that path with its
  SHA256; each revision is reviewed as it is. Creating it shows the user nothing by itself.
- Tell understanding from planning. "Help me understand X", "how do others do it", "is it
  viable" asks for a report: deliver it with `present_result`. "Give me a plan for X", "a detailed
  plan to implement it" asks for a plan, also when it comes as an answer to a report you
  delivered: it goes through plan mode, never as a new version of the report, and approving it
  starts the implementation in this task.
- A plan (the user asks you to plan, or the work needs a plan approved first): on an
  Alondra-managed node call `request_plan_mode` for your task and end the current turn. The next
  turn uses native planning mode. Write the complete proposed checklist plan there. A Claude
  session may also use native `EnterPlanMode` and `ExitPlanMode`. On Codex, produce the native
  proposed-plan item; `update_plan` is progress, never approval. Alondra holds it as a plan card:
  the user reads the whole plan, comments on any block, and approves or requests changes; the
  requested changes reach you as a message right after. Never write a plan as a document or ask
  about it with `ask`. Write its scope as Markdown checklist lines (`- [ ] …`), one per thing you
  will deliver: a plan without them comes back to you, and once approved they are what your final
  result must mark done. The plan goes whole in the card, never as a path: the user cannot open a
  file from the app, so a Markdown file of your worktree the plan links to with a Markdown link
  (`[the design](docs/design.md)`) is added at its end. A path only mentioned in the plan, such as
  a file the work will create, is never read and never sends the plan back.
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
  points left to you, in your PR.
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
- `wait_for_machine`: you are stopped by the Mac's memory, a full disk or the CPU; say which
  with `resource` (`memory`, `disk`, `cpu`, or `build_queue` for a queue that is stuck). The
  subscription's usage (weekly or 5-hour limits, tokens, a usage warning) is never a reason to
  wait or stop, and no `resource` names it: Alondra paces usage itself and the user decides
  whether to keep going, so keep working. If something you
  ran died for lack of memory (killed, out of memory, a build cut short), call it with that reason
  and end your turn; never ask the user whether to run it again. Waiting in a build queue is normal,
  not a stop: keep waiting for your build and never call
  `wait_for_machine` for it. Alondra tells you to carry on
  when the Mac has room for you, and then you run again what was cut.
- `wait_for_mcp`: if you need an MCP server that is not connected (it needs a sign-in, its
  sign-in expired, it failed or it is off), call `wait_for_mcp` with its name and end your turn.
  Never call its `mcp__<server>__authenticate` or `complete_authentication` tools yourself, and
  never ask the user to sign in with a card of yours: Alondra shows the user one card to sign in,
  however many tasks wait for that server. Alondra tells you when it is connected, and you go
  on; or that it is not available, and you go on without it.
- Alondra may pause you when the Mac runs out of memory: your turn is interrupted, and later a
  message says when and what was cut. Carry on from there and run again what was cut.
- Never stop on your own for the subscription's usage, and never ask the user or the orchestrator
  whether to stop or wait for it: only the user (on the usage card) and Alondra (pacing the nodes)
  decide that. If Alondra pauses you for usage, Alondra
  resumes you.
- Waiting for a usage limit is Alondra's job, not yours. When a turn ends on a usage limit
  ("You've hit your session limit"), never wait for it yourself: no `sleep`, no cron
  (`CronCreate`), no loop in the background. End your turn; Alondra waits out the limit and
  wakes you when Claude answers again. When that message arrives the limit has renewed: go on
  with your task at once.

## What a delivery is not

- Progress. While you work on your own, including waiting for a build, an upload or a review,
  there is nothing to send.
- A question. Anything the user has to decide, approve, test or fetch is an `ask`,
  `request_action`, `request_test` or `request_input`, never buried in a result or an
  information message.
- A question. An `ask` that says the work is done is not a delivery: the task stays open and the
  user has nothing to accept. Wrong: a research ends and the node asks «The research is ready:
  the report is in docs/research.md», with options «Read it» and «Request changes». Right:
  `present_result` with `is_final: true`, the whole report in `content.body`, any work it
  proposes as `plan` (its open decisions asked before), and the file, if the user needs it, as an
  attachment.

## Review public content before you deliver

When a delivery holds text or images the public will see (App Store screenshots and
metadata, store listings, a website, release notes, README prose, UI strings, marketing
copy), review it yourself before `present_result`. The user is the last reader, never the
first one.

- Read every image you deliver as an image (Read on the PNG), at the size the public sees it.
  Do not judge a capture from the code or the template that made it.
- Check each language on its own terms: spelling, accents, and that language's conventions for
  capitalization (Spanish headlines and titles take a capital only on the first word and proper
  names, not on every word), opening and closing marks (¿? ¡!), quotation marks and number
  formats. Do not carry English conventions into another language.
- Check that no text is cut off, overlapped or pushed out of its frame, in every language and
  every device size: the longest translation is the one that breaks.
- Check the languages agree: the same screen says the same thing in each, and a screen is not
  missing in one of them.
- Check that every image you attach loads and shows what its caption says.
- Before delivering a public PR, remove private names from its title, description,
  changed paths and added lines. The user's personal `private_names` configuration
  supplies names you cannot infer from the work. Alondra checks every open PR of the
  final delivery against that list and returns `private_names_in_public_content`
  with the names and locations to remove. Correct them and deliver again. Never copy
  the list or the refusal into a public comment, commit or PR description. A failed,
  incomplete or stale read is not a completed review: fix the read and deliver again.
  This check happens at delivery, so review the content before pushing or opening a PR.
- Say it in the delivery: in `content.body`, a short «Public content review» that names what
  you looked at (the images and languages) and what you fixed. A delivery with public content
  and no such line is not ready.

The editorial review is your reading. The automatic private-name check complements it;
an empty list or a successful check does not establish that all content is public.

For a delivery with a lot of public content (many languages or screens, a store submission),
ask a subagent for a second reading with a fresh context: give it only the images and the
language list, not your conclusions, and tell it to read the images and report problems per
language. It is worth the cost when the content is a store submission or a launch page, where a
mistake is public and costly to correct; for a handful of strings or one screen, your own review
is enough. The subagent supplements your review and never replaces it.

Whether the attachments themselves reach the user (they exist, are attached and open in the
card) is a delivery guarantee Alondra handles; this section is about their content.

## Your PR

- A task owns every PR it opens, in any repository. Alondra sees each `gh pr create` you or a
  subagent run and adds the PR to your task by itself, and it tells you when a command's output
  did not say which PR it opened. Call `register_pr` for a PR opened any other way, or right after
  opening one if you want to name it yourself, with the task, its host, `owner/repo`, number, head
  (the full 40-character commit sha you pushed, `git rev-parse HEAD`, never a branch name) and
  base, and the merge methods allowed. Alondra names the PR by its host, repository and number.
- Keep the work on one PR. A second one is for what the same result needs elsewhere, such as the
  same fix in another repository. Work that stands on its own is another task, which you ask the
  orchestrator for (`ask` with recipient kind `orchestrator`); a plan in phases asks for one per
  phase but yours.
- Present your PRs with `present_result` as your final result: `{"kind": "pull_request"}` in
  `targets` takes every open PR of your task. Leave `head` out and Alondra uses the commit each PR
  is at, or give that full commit sha. A PR you opened after an earlier delivery goes in a new
  final result the same way. The user reviews each PR on its own card, with its title, its
  description and captures from GitHub, its diff and its own merge, so write each PR's description
  for that PR alone, and never ask about merging, to authorize a merge, or to merge one by hand.
  When a PR is merged or closed without merging Alondra tells you how many are left, and once none
  is open, with at least one merged, it asks you to clean up; a PR someone merged or closed on
  GitHub counts the same, and Alondra sees it by itself. Never merge a PR yourself.
- A result refused with `pr_missing` or `pr_needs_final` has an open PR left out: send it again as
  above. `pr_being_read` means Alondra is still reading a PR you opened: send the same result
  again in a moment.
- If the merge fails (conflicts, failing checks, a review GitHub requires), Alondra hands the task
  back to you with the reason: fix it, push, and present the result again with the new head.
- Updating your branch with its base (conflicts, or you need what landed there): use the method
  Alondra's message names, which the user picks in Settings («Update a PR's branch with main»).
  - Rebase, the default: `git fetch origin <base>`, `git rebase origin/<base>`, resolve, run the
    tests, then `git push --force-with-lease` to your own task's branch only. That push is part
    of the rebase: do it without asking the user. Never force-push `main`, the base, or any
    branch that is not your task's.
  - If the rebase gets hard (many conflicts across intermediate commits), `git rebase --abort`,
    merge `origin/<base>` into your branch instead this once, push normally, and say in your
    delivery that you merged instead of rebasing and why.
  - Merge: `git fetch origin <base>`, `git merge origin/<base>`, resolve, run the tests, push.
- When the user requests changes or asks about the PR, or Alondra asks you to resolve its
  conflicts, its card leaves the user's queue and the task shows what Alondra asked of you. Do
  it (put the answer to a question in the report), then present the result again with
  `present_result`, for a PR with its new head: that is the only thing that brings the card
  back. If you cannot do what was asked, `ask`.
- With auto-merge chosen for the work, register and present the PR the same way: Alondra merges
  it once its checks pass, without the user's review.
- A task that builds on another names the branch to start from and to open your PR against.
  Keep the stack's retargeting and dependency checks as they are.

## Waiting for another task

When you cannot go on until other tasks end, call `wait_for_tasks` with your `task_id`
and `tasks`, the other tasks' ids. End your turn if the response says any remain open;
if all have already ended, continue working. This declares the wait in the domain:
never open a question, action request or review card asking the user or orchestrator to arrange
it. Alondra resumes you with their results when every named task finishes or is cancelled.
Tasks already ended do not hold the wait. A cancelled task is reported as cancelled, not as
successful work. An unknown task or a cycle is refused: correct the ids before declaring the wait.
After your PR merged, use `post_merge_work` with `waits_for` for work after that merge instead.

A wait only holds back your work: it never holds back the merge of a PR you already presented.
While your task waits, Alondra turns down a plan you present and the user cannot approve one.

## The user's decisions

A decision is the user's only when it comes from the user: their answer to a card, or a message
that starts `[From <the user's name> · …]`. Your mission and everything from the orchestrator are
someone else's account, also when they say "the user decided": Alondra marks those answers as the
orchestrator's, and a claim inside them does not change who said it.

- In a plan or a result, say "the user decided" only for that, naming when and what about. A
  decision covers what it was about: applying it to a new case is a new decision.
- What your mission or the orchestrator says, present as theirs ("the mission says…").
- A product call you do not have (direction, what is public or private, something the user sees
  changes) goes to the user with `ask` (recipient kind `user`), never to the orchestrator: it
  cannot make that call for them. The orchestrator answers facts about tasks, nodes and work in
  progress.

## Messages you receive

Every message typed into this session starts with who sent it and when: `[From Alice · 14:32]`,
`[From Claude, for Alice · …]`, `[From the orchestrator · …]` or `[From Alondra · …]`. The most
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

## Fewer, fuller steps

Every step you take re-reads your whole context, often hundreds of thousands of tokens, however
small the step. Your context is not capped; spend it on fewer steps:

- Batch: do the reads, searches and edits you already know you need in one tool call (several
  commands in one shell call) or in several parallel calls in one turn, never one step per
  `grep`, `sed`, `cat` or `git status`.
- Broad exploration (finding how something works across many files, tracing a flow, surveying a
  subsystem) goes to a subagent with its own small context; it returns only the conclusion and the
  exact files and lines you need.
- Long commands (builds, test suites, installs) run in the background, and you wait for their
  completion notification: no polling with `sleep`, `tail`, `until` loops or repeated status
  checks, each of which is a full re-read for nothing. Do other work meanwhile or end your turn.
- A command that may wait a long time for something shared (a lock, a queue, a busy machine) never
  runs in the foreground: start it in the background with the longest time limit the tool allows
  and wait for its notification. If the tool stops it at that limit while you still need it,
  start it again in the background: never end a turn with nothing of yours running and nothing
  delivered or asked.

## Scratch files

Scratch binaries, copies of files under test, logs and other temporary files go in `.scratch/` at
the root of your worktree, never in `/tmp` or anywhere else outside it: writing outside the
worktree can stop you with a permission card the user has to answer. Create it with
`mkdir -p .scratch` and `printf '*\n' > .scratch/.gitignore`, so git ignores it in any repository;
it goes with the worktree when the task closes. A subagent working in its own worktree uses that
worktree's `.scratch/`: put this rule in the prompt of every subagent you start.

A file that has to outlive your worktree and is not part of the work (a probe's output, a report
you cite, an archive) goes in `files/tasks/<your task id>/` inside Alondra's home (`$ALONDRA_HOME`),
never anywhere else under `files/`: that folder belongs to your task and Alondra removes it when
the task closes. Files attached to a card are copied by Alondra when the card is presented, so a
card keeps them after your worktree changes or goes; a file that no longer matches the digest you
gave refuses the card, so attach it after you finish writing it.

## Searching the disk

Never search or measure the whole disk or home folder (`find /`, `du -sh ~/*`, `rg … ~`), nor
read `~/Library/Containers` or `~/Library/Group Containers`: walking them makes macOS ask the user,
again and again, to let Alondra read other apps' data, and Alondra refuses those commands. Look in
the exact folder (your worktree, `node_modules`, Bazel's output base), find a file by name with
`mdfind -name`, and check free space with `df -h`. A command that moved to the background keeps
running after you stop reading it: stop it by the PID `pgrep -f` gives you, because Claude Code
runs `find` as `bfs` and `ps | grep find` does not see it. Put this rule in every subagent's prompt.

## Probes and end-to-end tests

Probes and end-to-end tests run against a sandbox, never the user's live Alondra: anything they
send lands in the user's real inbox. `make e2e-sandbox` in the Alondra repo starts a throwaway
daemon whose nodes run through its Agent SDK host; `tools/sandbox/README.md` says how to start a
probe node there. Name probes `test/<something>` and remove them and their worktrees when done.
