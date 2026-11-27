+++
id = "antigravity-quota-holds-its-group"
title = "A used-up Antigravity quota holds the task until its group resets"
status = "accepted"
date = "2026-11-27"
supersedes = []
superseded_by = ""
+++

## Context

A Google-signed-in Antigravity account draws on several quota pools, one per group of models, each
with its own reset time. `agy` reports a used-up quota during a turn as the turn's text, not as an
error, so Alondra used to record the turn as completed with the error as its answer, and the
account kept taking tasks until the next usage reading, up to a day later.

## Decision

The user decided on 2026-11-27: when a turn ends because the quota of its model's group ran out,
the turn does not count as completed; the task waits until that group's reset time and then goes on
by itself. The account keeps taking tasks whose models draw on a group with room. When the reset
time is unknown, the task waits for the next usage reading. The account is held as a whole only when
every group ran out. Accounts billed per use (`api_key`, `cloud_project`) are never held for quota.
The decision is made on a typed signal, never on the text of the turn.

## Consequences

Usage is read while nodes on the account run, not only in the daily check. The task's waiting state
says which group it waits for and when it resets.

## Confirmation

Not built yet. The change that builds it adds its scenarios to `antigravity.usage`: a used-up quota
holds its group until its reset, and one spent group never holds the account.
