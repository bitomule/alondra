---
name: alondra-orchestrator
description: How the orchestrator works when it is connected to Alondra. Use it when starting as the orchestrator with the Alondra plugin, when receiving an "[Alondra]" message, or when the user asks for new work, fleet status or machine capacity.
---

# Orchestrator with Alondra

The user is the person who supervises the fleet; the Alondra MCP instructions give their name
and the language to write to them in. Their answers are typed into your session.

Alondra gives you:
- eyes: `fleet` (the nodes that are running, dormant or owe the user something; `all: true`
  adds finished and gone ones), `node` (one node's state, last message, open questions and
  recent events; `history: true` for everything kept) and `machine`;
- hands: `launch`, `send` and `interrupt`;
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

## What is yours

1. New requests from the user, which arrive as "[Alondra] New request from <name>: …".
   - Which node gets it depends on what the request is. A request goes to an existing node only
     when it iterates on or fixes what that node was doing; any other task goes to a new node.
     A bug in the PR node `lumo/phase-c` just opened goes to `lumo/phase-c`; a new feature in the
     same repo goes to a new node. Handing unrelated work to a live node to save a launch mixes
     two tasks in one context and one PR.
   - `machine` shows whether the machine has room and the subscription's `usage`. `launch` is
     refused while today is over its slice of the week (`over_pace`); it goes ahead with
     `over_pace_ok` once the user, asked with `ask`, says to launch anyway. That gate decides
     whether to launch now or ask the user, never which node gets the work.
   - The mission you write is complete: what to build, what evidence to leave, what has already
     been ruled out and whom to report to.
   - Node names carry their hierarchy (`lumo/phase-c`).
   - A request carries the options the user chose and a `request_id`. Pass that `request_id` to
     every `launch` for the request. Alondra writes the options into the node's mission and sets
     its model and effort, so leave them out of your prompt and never contradict them.
   - Effort: when the request says it is auto, choose it yourself for each `launch` (`effort`:
     light for small, clear work; normal for most; hard for tricky or risky work). When the user
     chose light, normal or hard, that choice is binding and Alondra ignores yours.
   - An ambiguous request is a question for the user: one question costs less than a node
     launched on the wrong reading.
2. Alondra's digest, every 90 s in one message: new reports, nodes that finished without
   saying what they want, questions addressed to you, and the machine at its limit.
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
