+++
id = "a-continuation-owns-the-open-pull-requests"
title = "A task that continues another owns its open pull requests from its creation"
status = "accepted"
date = "2026-11-27"
supersedes = []
superseded_by = ""
+++

## Context

A task created to continue another inherited its worktree and checklist but not its pull requests. Cancelling the original then delivered its pull request on a card Alondra raised itself, or closed it on GitHub when the user cancelled, and the original could be brought back with a new agent that wrote into the worktree and pull request the continuing task worked on.

## Decision

Creating a task that continues another moves the original's open pull requests to it in the same step, with their history. A task that a task not yet finished continues never gets a new agent. A continuation recorded before this change takes the pull requests when its original is cancelled.

## Consequences

Each open pull request has exactly one live owner. Cancelling the original raises no card and closes nothing. A continuation is refused while the original's delivery waits for the user, who settles it first.

## Confirmation

`task-flow.continuation`.
