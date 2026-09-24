---
name: alondra
description: How to talk to the user and to the orchestrator through Alondra. Use it when you need a decision from the user, when you deliver something, or when you are stopped for lack of machine resources.
---

# Alondra

The user is the person who supervises the fleet. Their name comes in the instructions of the
Alondra MCP server. Their answers are typed into your session as if they wrote them there. Write every question and report for them in the language the MCP instructions name, even if your conversation is in another language.

You are connected to Alondra. You do not have to report your state: Alondra sees it on its own.
What you write in your terminal does not reach the user; only Alondra's tools do.

- **You need the user to decide something** → the `ask` tool. One question per decision,
  one-line title (it is what they see in the notification), all the context in `detail`,
  options, PRs in `links` and files in `attachments`. Then end your turn: Alondra types the
  answer into this session. Do not leave the question loose in your last message.
- **The user has to do something outside Alondra** (test on the iPhone, get a key, check App
  Store Connect) → the `request_action` tool: a one-line title, the steps in order and the links
  where to do it. They get a "Your turn" card and answer "Done" or explain why they cannot;
  end your turn and Alondra types that into this session. Never leave it written in your
  terminal: nobody reads it there.
- **Menu questions** (`AskUserQuestion`) also reach the user; use them for short choices.
- **You are done** and nothing is pending → `report` with `kind: "final"`. This is the normal
  way to end a task.
- **A result or finding the user must know now** → `report` with `kind: "notice"`, with the
  result itself in the title.
- **A document for them to read** (plan, long report) → `report` with `kind: "document"` and the
  path in `attachments`. If you also need them to **approve** it, it is not a report: it is an
  `ask` with the document attached.
- **A question right after your own report on the same thing** → cite the report by number in
  the question (`informe #N`), or attach the same file or link the same URL: Alondra shows the
  report inside the question as one card, as long as it is unread and at most 30 minutes old.
- **Stopped by the machine** (RAM, build queue, usage limit) → `wait_for_machine`.
- **You opened a PR** → Alondra sees `gh pr create` and gives it its own card, where the user
  merges, requests changes or sets it aside, and counts you as waiting for the user until they do.
  Do not send a report just to say the PR is ready, and do not `ask` whether to merge it. When
  you are done, your `report` with `kind: "final"` links the PR in `links`: it is shown inside
  the PR's card, not as a card of its own. A question about something else in that
  PR goes with the PR in `links`, and it is shown inside the PR's card.
- **A question of yours no longer needs an answer** (settled in the terminal, the PR it was
  about got merged, you moved on) → `withdraw` it with its number, so its card does not stay.
  Alondra already withdraws questions that link to a PR once that PR is merged or closed.

## What never goes to the user

- **Progress.** "Still working", "I do not need you yet", "I will tell you when it is done",
  step lists of work under way: none of that is a report. While you work on your own
  (including waiting for a build, an upload or a review), send nothing. Alondra rejects these
  reports, and you lose a turn.
- **Questions inside a report.** If a report asks the user to decide, approve, test or fetch
  something, Alondra delivers it flagged and the question gets lost among the reading: send it as
  `ask` (or `request_action` if it is something to do).

## Probes and end-to-end tests

- **Never run a probe or an end-to-end test against the real alondrad** (port 47800): whatever
  it reports or asks lands in the user's inbox. Alondra refuses `report`, `ask` and
  `request_action` from nodes whose name starts with `test/` unless it runs as a sandbox.
- Start a sandbox with `make e2e-sandbox` in the Alondra repo: it runs a throwaway alondrad with
  its own data directory and port, and prints the `claude --bg --name test/<probe> --plugin-dir …
  --settings …` line that points a probe node at it instead of the real one. Name probes
  `test/<something>`, close them with `claude rm` and stop the sandbox when done.
