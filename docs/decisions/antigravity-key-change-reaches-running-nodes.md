+++
id = "antigravity-key-change-reaches-running-nodes"
title = "Removing an Antigravity key stops its running nodes; replacing it applies when their turn ends"
status = "accepted"
date = "2026-11-27"
supersedes = []
superseded_by = ""
+++

## Context

`agy` reads its key from its environment when it starts and keeps it for the whole session. A user
who removes a key that leaked expects it to stop working at once; a user who replaces a key expects
the new one to be used without losing the work in progress.

## Decision

The user decided on 2026-11-26: removing an account's key ends the `agy` of every node running on
that account right away, and those tasks wait for the account to be signed in again. Replacing the
key lets each running turn finish, and the next turn of each node starts an `agy` with the new key.

## Consequences

The confirmation of «Remove key» says that running nodes on the account stop.

## Confirmation

Not built yet. The change that builds it adds its scenarios to `antigravity.key`: removing a key
stops the running nodes, and replacing it applies at the next turn.
