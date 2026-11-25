+++
id = "ledger-holds-fingerprints-only"
title = "The ledger holds fingerprints and verdicts, one small file each"
status = "accepted"
date = "2026-11-25"
supersedes = []
superseded_by = ""
+++

## Context

What was validated has to be visible to continuous integration without the tool that validated it, survive parallel branches and be safe to commit to a public repository.

## Decision

The ledger is a folder of small TOML files, one per suite, scenario and reference section. A file holds fingerprints and verdicts only: no dates, no names, no free text. The reasoning of a judge and the evidence files stay with whoever ran the validation.

## Consequences

Two branches only collide when they validate the same thing, and validating again resolves it. The ledger is a format that must keep reading: files written by an earlier build are fixtures of the engine's tests.

## Confirmation

`spec-engine.ledger`.
