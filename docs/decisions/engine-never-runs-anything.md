+++
id = "engine-never-runs-anything"
title = "Validating never runs a command or starts an agent"
status = "accepted"
date = "2026-11-25"
supersedes = []
superseded_by = ""
+++

## Context

A check that launches agents or commands by itself hides cost and hides who judged what. Continuous integration needs a check that only fails and says what is missing.

## Decision

Validating compares the ledger with the current fingerprints and lists what is missing. It never executes a command, never starts an agent and never writes. Suites are run by the caller from the plan the engine gives, and judgments are recorded by whoever made them.

## Consequences

In continuous integration a missing validation is a failure. Who is allowed to record a verdict is decided by the caller, not by the engine.

## Confirmation

`spec-engine.validation` and `spec-engine.run-plan`.
