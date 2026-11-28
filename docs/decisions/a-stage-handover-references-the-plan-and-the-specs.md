+++
id = "a-stage-handover-references-the-plan-and-the-specs"
title = "A stage handover is a short packet that references the plan, the task's record and the specs"
status = "accepted"
date = "2026-11-28"
supersedes = []
superseded_by = ""
+++

## Context

A new stage of a task starts from a packet Alondra writes, not from the conversation before it. That packet held everything inline: the approved plan, every approved spec described scenario by scenario, the validation and the task's whole record. It had to fit a budget of 64,000 tokens whenever the account's catalog did not give the destination model's window, which is the case for Claude, Codex and Antigravity. A task with a long plan and many specs needed about 65,600 tokens, its handover from the validator to a new implementer was refused three times, and the task stopped on a start problem whose "Try again" could only fail the same way.

## Decision

The packet holds only what the role does, the worktree, branch and pull requests, and the validation of its role, with every failed validation whole. The rest goes by reference. The approved plan goes to `files/tasks/<task>/approved-plan.md` and the task's record (request, every message and answer of the user, decisions, earlier work) to `files/tasks/<task>/task-records.md`, both in Alondra's home, and the packet names them. The conversation goes nowhere. The approved specs go by their paths in the worktree, where Alondra already writes them, or, when the project keeps its specs out of the repository, by their paths under `files/tasks/<task>/specs/`. The budget for a destination model with no known window rises from 64,000 to 100,000 tokens.

## Consequences

The packet no longer grows with the plan, the specs or the task's history. The new agent reads them from files, as it already reads the code. The files live in the task's folder and go with the task. A packet that does not fit is not expected, so the start problem card is unchanged.

## Confirmation

`stage-handover.packet`, `stage-handover.budget`.
