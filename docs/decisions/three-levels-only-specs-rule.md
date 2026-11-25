+++
id = "three-levels-only-specs-rule"
title = "Three levels of documentation, and only specs rule"
status = "accepted"
date = "2026-11-25"
supersedes = []
superseded_by = ""
+++

## Context

People need explanations at a higher level than scenarios, but prose that falls behind misleads whoever reads it, and an agent follows a stale document even when the code disagrees.

## Decision

There are three levels. Visual explanations are generated when they help and never stored as truth. Reference prose lives in the repository, each section anchored to the specs, boundaries and decisions it explains, and goes stale when they change. Specs, boundaries and decisions are the only source of truth and the only input of a regeneration. Every packet handed to an agent says that a spec wins over any other text.

## Consequences

Prose cannot hold a requirement of its own: a section that explains nothing is a problem. Lasting decisions are records the engine reads, and a spec may only cite an accepted one.

## Confirmation

`spec-engine.reference`, `spec-engine.decisions` and `spec-engine.packet/states-that-specs-win`.
