+++
id = "validations-carry-over-to-a-continuation"
title = "What a validator recorded counts for the tasks that continue its task"
status = "accepted"
date = "2026-11-28"
supersedes = []
superseded_by = ""
+++

## Context

A task that continues another works in the same worktree and often on the same commit. The ledger on that commit already holds what the original's validator recorded, so the spec engine lists nothing to validate. The guard of the delivery, though, accepted only ledger entries a validator of the delivering task had recorded. The continuing task was told there was nothing to validate when it asked for validation, and that its ledger was unrecorded when it delivered, with no way out.

## Decision

When the guard of a delivery checks a ledger entry, a record of a validator counts if the delivering task made it, or any task it continues, directly or through a chain of continuations, with the same subject, kind, fingerprint and verdict. The records are read when the delivery is checked; nothing is copied into the continuing task.

## Consequences

Work validated once is delivered without being validated again after it moves to another task. Every ledger entry of a delivery is still backed by a validator's record at that fingerprint. Entries written by hand, entries whose verdict or fingerprint differs, and entries only an unrelated task recorded are still refused. A continuation stored before this change delivers once Alondra is updated, with no stored data to migrate.

## Confirmation

`task-flow.continuation`, scenarios `validated-work-is-delivered`, `whole-chain-counts`, `carried-entry-must-match`, `unrelated-records-do-not-count` and `stored-continuation-delivers`.
