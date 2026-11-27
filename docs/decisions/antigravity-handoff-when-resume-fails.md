+++
id = "antigravity-handoff-when-resume-fails"
title = "A node whose Antigravity session cannot be resumed starts again with the task's handoff"
status = "accepted"
date = "2026-11-27"
supersedes = []
superseded_by = ""
+++

## Context

A node's host keeps the ACP session it opened and resumes it when the node goes on in a new host.
`agy` can forget a session: after an update, or when its folder is cleared. A new session then
starts with only the node's instructions and its next input, and the node no longer knows where it
was.

## Decision

The user decided on 2026-11-27: whenever a node's stored session cannot be resumed, the new session
starts with the task's handoff, the same summary a task carries when it moves to another provider,
ahead of the next input. A session that resumes takes no handoff.

## Consequences

The daemon gives the host the handoff on every relaunch and adoption, not only on a provider
transition.

## Confirmation

Not built yet. The change that builds it adds its scenario to `antigravity.sessions`: a session
`agy` forgot starts with the handoff.
