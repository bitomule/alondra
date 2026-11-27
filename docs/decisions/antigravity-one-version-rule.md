+++
id = "antigravity-one-version-rule"
title = "The account check and the Antigravity host accept the same agy"
status = "accepted"
date = "2026-11-27"
supersedes = []
superseded_by = ""
+++

## Context

The account check and the Antigravity host both decide whether the `agy` on the Mac is one Alondra
can run. They read its version and its name by different rules, so an account could check signed in
while every node on it was refused.

## Decision

The check and the host apply one rule to `agy`'s version and name. A version is read by its numbers:
the user decided on 2026-11-27 that a version with a suffix, such as `1.5.14-beta`, counts as
`1.5.14`. The lowest version Alondra runs is `1.5.12`. An `agy` the check accepts is never refused by
the host for its version or its name, and the other way round.

## Consequences

Users on `agy`'s test channel can run nodes. Both sides test against the same list of versions and
names.

## Confirmation

Not built yet. The change that builds it adds its scenario to `antigravity.account-check`: the
check and the host accept the same `agy`.
