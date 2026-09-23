---
name: alondra
description: How to talk to the user and to the orchestrator through Alondra. Use it when you need a decision from the user, when you deliver something, or when you are stopped for lack of machine resources.
---

# Alondra

The user is the person who supervises the fleet. Their name comes in the instructions of the
Alondra MCP server. Their answers are typed into your session as if they wrote them there.

You are connected to Alondra. You do not have to report your state: Alondra sees it on its own.
What you write in your terminal does not reach the user; only Alondra's tools do.

- **You need the user to decide something** → the `ask` tool. One question per decision,
  one-line title (it is what they see in the notification), all the context in `detail`,
  options, PRs in `links` and files in `attachments`. Then end your turn: Alondra types the
  answer into this session. Do not leave the question loose in your last message.
- **Menu questions** (`AskUserQuestion`) also reach the user; use them for short choices.
- **You are done** and nothing is pending → `report` with `kind: "final"`. This is the normal
  way to end a task.
- **A result or finding the user must know now** → `report` with `kind: "notice"`, with the
  result itself in the title.
- **A document for them to read** (plan, long report) → `report` with `kind: "document"` and the
  path in `attachments`. If you also need them to **approve** it, it is not a report: it is an
  `ask` with the document attached.
- **Stopped by the machine** (RAM, build queue, usage limit) → `wait_for_machine`.

## What never goes to the user

- **Progress.** "Still working", "I do not need you yet", "I will tell you when it is done",
  step lists of work under way: none of that is a report. While you work on your own
  (including waiting for a build, an upload or a review), send nothing. Alondra rejects these
  reports, and you lose a turn.
- **Questions inside a report.** If a report asks the user to decide, approve, test or fetch
  something, Alondra rejects it: send it as `ask`.
