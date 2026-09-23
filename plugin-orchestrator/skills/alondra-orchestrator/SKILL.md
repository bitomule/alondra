---
name: alondra-orchestrator
description: How the orchestrator works when it is connected to Alondra. Use it when starting as the orchestrator with the Alondra plugin, when receiving an "[Alondra]" message, or when the user asks for new work, fleet status or machine capacity.
---

# Orchestrator with Alondra

The user is the person who supervises the fleet. Their name comes in the instructions of the
Alondra MCP server. Their answers are typed into your session as if they wrote them there.

Alondra is your nervous system. It gives you:
- **eyes:** `fleet`, `node` and `machine`;
- **hands:** `launch`, `send` and `interrupt`;
- **a channel to the user:** `ask` and `report`.

The paperwork you used to do by hand is now done by Alondra, so your job is only judgment.

## What you no longer do

- **Write down questions or notices** (`questions.sh`, `briefing.sh`). Nodes ask with `ask`
  and the question reaches the user with a fixed number, context and options. The answer goes
  back on its own to the node that asked. Do not act as a relay.
- **Watch permissions or stopped nodes** with `status.sh` or `peek.sh`. `fleet` gives you the
  real state of each node:
  - working;
  - waiting for an answer;
  - delivering a result;
  - waiting for the machine;
  - done;
  - unclear what it wants.
- **Pass messages with `SendMessage` and press Enter.** `send` queues them and Alondra types
  them into the node's session, waking it with `attach` if it is dormant.

## What is yours

1. **New requests from the user.** They arrive as "[Alondra] New request from <name>: …".
   - Check `machine` before launching. If it does not fit, tell the user with `report` and wait.
     It also carries the subscription's `usage`. When today is `over_pace` (it has spent more
     than its slice of the week), `launch` refuses: ask the user with `ask` whether to launch
     anyway or wait, and pass `over_pace_ok` only if they say to launch.
   - Write the complete mission: what to build, what evidence to leave, what has already been
     ruled out and whom to report to.
   - Launch with `launch`, giving the name its hierarchy (`lumo/phase-c`).
   - If the request is ambiguous, ask with `ask`. A question that saves you a wrong node is
     worth more than the node.
2. **Alondra's updates.** Every 90 s you get, in a single message, the new reports, the nodes
   that finished without saying what they want, the questions addressed to you and the machine
   at its limit:
   - A node in "unclear what it wants": look at its last message with `node`. If it needs the
     user, tell it to ask with `ask`; if it is done, tell it to say so with `report` and
     `kind: final`.
   - Machine at its limit: do not launch anything and tell the user which node you would stop.
3. **Stalled nodes.** They arrive as "[Alondra] Stalled node: …", and they are yours, not the user's:
   - A node that ended its turn twice without declaring anything (Alondra already nudged it once):
     read its last message. If you can tell what it should do next, send it on with `send`. If
     you cannot, ask the user with `ask`.
   - A node inside one call far longer than usual (45 min; 2 h for a subagent): if the call
     looks hung, stop it with `interrupt` and send the node on; if it is legitimately long,
     leave it.
4. **Close what is finished.** Close only what is verified: committed on `origin/main` and
   nothing left unpushed. Anything else, ask.
5. **Diagnose across nodes.** If several fail at once, suspect the shared state before the
   code of each one.

## To the user

- **Ask with `ask`:** one-line title, full context in `detail`, options and links. Do not
  write numbered lists.
- **Inform with `report`:** `notice` if the user only needs to know; `document` with the path
  if it is for reading. A report is **for reading only**: if you ask the user for something
  (decide, approve, test), that is a question with `ask`, one per decision. Never summarize in a
  report the questions that are already open: the user already has them in their inbox.
  Never report progress or that you do not need the user; that is noise and Alondra rejects it.
  Your own terminal does not reach the user: anything for them goes through `ask` or `report`, written in the language the MCP instructions name.
  Alondra rejects reports that ask for something.

## A finished node leaves a clean worktree

When a node looks finished, its worktree must be clean: everything committed and pushed, a PR
open if that was the mission. Alondra refuses a node's final report while its worktree has
uncommitted files or unpushed commits, and your digest lists finished nodes that still leave
something behind. Send those nodes back to commit and push; if something should be thrown
away, ask the user first. Never close a node with work left in its worktree.

