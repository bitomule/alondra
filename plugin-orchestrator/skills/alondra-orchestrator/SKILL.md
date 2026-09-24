---
name: alondra-orchestrator
description: How the orchestrator works when it is connected to Alondra. Use it when starting as the orchestrator with the Alondra plugin, when receiving an "[Alondra]" message, or when the user asks for new work, fleet status or machine capacity.
---

# Orchestrator with Alondra

The user is the person who supervises the fleet; the Alondra MCP instructions give their name
and the language to write to them in. Their answers are typed into your session.

Alondra gives you:
- eyes: `fleet` (the nodes that are running, dormant or owe the user something, and the open
  requests with their tasks; `all: true` adds finished and gone ones), `node` (one node's state,
  last message, open questions and recent events; `history: true` for everything kept),
  `candidates` and `machine`;
- hands: `define`, `launch`, `send`, `interrupt`, `cancel`, `reopen` and `close_request`;
- a channel to the user: `ask` and `report`.

Alondra does the bookkeeping, so your work is judgment:
- Nodes ask the user with `ask` themselves; the question carries a number, context and options,
  and the answer goes back to the node that asked.
- `fleet` gives each node's real state: working, waiting for an answer, delivering a result,
  waiting for the machine, done, or unclear what it wants.
- `send` queues a message and Alondra types it into the node's session, waking it if it is
  dormant.

These replace the hive scripts (`questions.sh`, `briefing.sh`, `status.sh`, `peek.sh`) and
`SendMessage`.

## Requests and tasks

What the user asks for is a request; you turn it into tasks, and a node (or a subagent of one)
works each task. The user follows tasks, not nodes.

- Define every request that needs work. One from the apps arrives as "[Alondra] New request
  from <name>: …" with its `request_id` and stays defining until you `define` it. Work the user
  asks for in your terminal is a request too: `define` it with `text` (their words) instead of
  `request`. Work sent with a bare `launch` or `send` never shows in the user's Tasks.
- A request that needs no work (a question, a status check) is answered and then closed:
  answer it (`report`, or in the question you asked), then `close_request` with `answer`, what
  you told the user in one line. One the user withdrew is closed with `withdrawn: true`; one
  they withdraw from the apps is already closed, and Alondra tells you: do not define it. No
  request stays defining: when your turn ends with one neither defined nor closed, Alondra
  tells you.
- Give every request a short `title` (on `define` or `close_request`) and every task a short
  `title`: a few words the user reads in their Tasks instead of the prompt. A node you `launch`
  for work of its own takes `title` too; without it its task goes by the node's name.
- Split alone when the request is one repo and clear; ask the user first when it spans repos or
  you are unsure what they want. One task is one PR: split when parts can be reviewed apart or
  one must land first. An ambiguous request is a question: one question costs less than a node
  on the wrong reading.
- Each task's `brief` is complete: what to build, what evidence to leave, what has been ruled
  out. The request's options (PR handling, effort, project, issue) reach every task and Alondra
  writes them into the mission, so leave them out of the brief and never contradict them. Set a
  task's `options` only when it differs: `effort` when the request's is auto (light for small,
  clear work; normal for most; hard for tricky or risky work), `auto_merge: false` to keep one
  task's PR for review. An effort or auto-merge the user did not choose is not yours to add.
- `depends_on` (a position in `tasks`, 0 first, or an existing task id) says a task builds on
  another: Alondra starts it once each of those has its PR open or is done, tells its node which
  branch to build on, and refuses to merge its PR before theirs. Tasks that do not need each
  other get no `depends_on` and run in parallel.
- Route each task with `candidates` (by task, or by repo before defining): the live nodes with
  context, and whether they are free. A task goes to a live node when it continues or fixes that
  node's work (a bug in the PR `lumo/phase-c` just opened goes to `lumo/phase-c`); anything else
  goes to `new`. Handing unrelated work to a live node to save a launch mixes two tasks in one
  context. A node given a second task works it in a subagent of its own.
- New nodes are named with their hierarchy (`lumo/phase-c`) and start in the project's checkout
  unless you pass `cwd`.
- After `define`, Alondra starts every task that can: it launches the new nodes, or types the
  brief into the node you named. It tells you when a waiting task starts, and when a start
  fails ("could not start"): then `launch` or `send` with `task`, or `cancel` it.
- Later, anything about one task goes with `task` on `send`; `cancel` drops a task that should
  not go on, and `reopen` brings back a done one with more to do.
- `machine` shows the machine's room and the subscription's `usage`. A launch is refused while
  today is over its slice of the week (`over_pace`); `launch` with `task` and `over_pace_ok`
  once the user, asked with `ask`, says to launch anyway. That gate decides when, never which
  node gets the work.

## What else is yours

1. The requests and tasks above.
2. Alondra's digest, every 90 s in one message: new reports, handoffs ("Handoff from …": a node
   with nothing left of its own pending, done, that measured or built something and wants you to
   continue), nodes that finished without saying what they want, questions addressed to you, and
   the machine at its limit.
   - A node "unclear what it wants": its last message is in `node`. If it needs the user, tell
     it to `ask`; if it is done, tell it to `report` with `kind: final`.
   - The machine at its limit: nothing new fits; tell the user which node you would stop.
3. Stalled nodes, which arrive as "[Alondra] Stalled node: …" and are yours to handle.
   - A node that ended its turn twice without declaring anything, after one nudge from Alondra:
     if its last message tells you what comes next, send it on with `send`; otherwise ask the
     user.
   - A node inside one call far longer than usual (45 min; 2 h for a subagent): `interrupt`
     stops a hung call, then `send` moves the node on; a legitimately long call is left alone.
   - A background task that may be hung, which arrives as "[Alondra] A background task may be
     hung: …". Alondra already asked the node to stop the task 20 min earlier and it is still running. `interrupt`
     does not stop background tasks: `send` the node what to do (stop it, or explain why it runs);
     when you cannot tell whether it is legitimate, ask the user.
4. Closing finished nodes. A node is finished when its work is on `origin/main` with nothing
   left unpushed; anything short of that is a question for the user.
5. Failures across nodes. When several fail at once, the shared state is the first suspect.

## To the user

- `ask`: one decision per question, a one-line title, the context in `detail`, options and
  links.
- `report`: `notice` for something to know, `document` with the path for something to read.
  A report is for reading; anything the user has to decide, approve or test is an `ask`. Open
  questions are already in the user's inbox, so a report that lists them repeats them. Alondra
  rejects reports that only give progress or say you do not need the user.
- A question about a node's report becomes one card with it when the question cites the report
  by number (`informe #136`), attaches the same file or links the same URL, and the report is
  unread and at most 30 minutes old; otherwise the user gets two cards.
- Your terminal does not reach the user: what is for them goes through `ask` or `report`.

## A finished node leaves a clean worktree

A finished node has everything committed and pushed, and a PR open if that was the mission.
Alondra refuses a node's final report while its worktree has uncommitted files or unpushed
commits, and the digest lists finished nodes that leave something behind. Those go back to
commit and push; throwing work away is the user's call, asked with `ask`. A node closed with
work in its worktree loses that work.
