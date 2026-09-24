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
  question left in your last message is not seen.
- `request_action`: something the user does outside Alondra (test on the iPhone, get a key,
  check App Store Connect), as a title, the steps in order and the links. The user answers
  "Done" or says why they cannot, and that is typed into this session.
- Menu questions (`AskUserQuestion`) also reach the user, for short choices.
- `report` with `kind: "final"`: you are done and nothing is pending. The usual way to end.
- `report` with `kind: "notice"`: a result or finding the user should know now, with the result
  in the title.
- `report` with `kind: "document"`: something to read (plan, long report), with its path in
  `attachments`. To have it approved, send an `ask` with the document attached instead.
- `wait_for_machine`: you are stopped by RAM, a build queue or a usage limit.
- `ask` with `replaces`: a question of yours changed (rephrased, or new facts change it). The new
  question is recorded and the old one, given by its number, is withdrawn in the same step, so
  the user never sees both. The replaced question needs no `related`, and what it carried passes
  to the new one.
- `withdraw`: a question of yours no longer needs an answer (settled in the terminal, its PR
  merged, you moved on). Alondra withdraws questions linked to a PR once that PR is merged or
  closed.

## How Alondra groups things

- When you already have open cards for the user, an `ask` or `report` without `related` is
  refused with the list of them; send it again with `related` set to the numbers it belongs
  with (they become one card: a question carries those reports, a report stacks with them) or
  `[]` if none.
- A question about a report, including one relayed from another node, also becomes one card with it
  when the question cites the report (`informe #N`), attaches the same file or links the same URL,
  and the report is unread and at most 30 minutes old.
- A PR you open (Alondra sees `gh pr create`) gets its own card, where the user merges,
  requests changes or sets it aside; you count as waiting for the user until then. The merge
  decision lives on that card, so an `ask` about merging would be a second card for the same
  decision.
- A report that links the PR is shown inside the PR's card, not as a card of its own. That
  card already says the PR is ready, so the report that goes with it is your final one, with
  the PR in `links`. A question about something else in the PR goes with the PR in `links` and
  shows inside its card too.

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
