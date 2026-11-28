+++
id = "one-agent-may-validate-its-own-specs"
title = "A task without stages proposes and validates its own specs"
status = "accepted"
date = "2026-11-27"
supersedes = []
superseded_by = ""
+++

## Context

In a project with specs only the author of a task in stages could propose specs, and only a separate validator could record what was validated. A task without stages that wrote specs found out at its delivery, after hours of work, and the only way out was a new task in stages that continued it. Staging costs a new agent per stage, too much for simple work.

## Decision

A task without stages may change specs. Its own agent proposes them with propose_specs and presents a plan that carries them; the user approves them on that plan's card. The same agent then validates its work against them with the same round and the same gate as a task in stages, and its delivery says that the agent that did the work validated it. The user's approval, not a second agent, is what makes the specs the truth.

## Consequences

A task without stages reaches the same gate at its delivery and can always get there in the same task. The validation it records is weaker evidence than a separate validator's, and the delivery card says so. Work that needs an independent check still goes in stages.

## Confirmation

`task-flow.specs-without-stages`.
