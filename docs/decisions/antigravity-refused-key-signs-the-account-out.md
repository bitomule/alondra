+++
id = "antigravity-refused-key-signs-the-account-out"
title = "A Gemini key Google refuses signs its Antigravity account out and holds its tasks"
status = "accepted"
date = "2026-11-27"
supersedes = []
superseded_by = ""
+++

## Context

Real `agy` accepts any Gemini API key when asked to authenticate, and reports what Google answers
afterwards as the text of a turn. A wrong, revoked or unfunded key therefore looked signed in, and
every task on it ended "completed" with Google's error as its answer.

## Decision

The user decided on 2026-11-26 that a Gemini API key is checked against Google's public Gemini API
when it is saved and in the daily account check, and on 2026-11-27 that a key Google refuses during
a turn is treated like a refused sign-in. In both cases the account becomes signed out and takes no
more nodes, and a task whose turn the refusal ended waits on the card "Sign in to Antigravity outside
Alondra", whose "Try again" goes on once the account has a key Google accepts. The decision is made on
a typed signal (an HTTP status of the public API, or a status `agy` reports in a structured form),
never on the text of a turn.

## Consequences

The accounts screen says the key was refused. The fake `agy` refuses a key only the way real `agy`
does.

## Confirmation

Not built yet. The change that builds it adds its scenarios to `antigravity.account-check` (a key
Google refuses signs the account out) and `antigravity.signed-out` (a key refused during a turn
holds the task).
