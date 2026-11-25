+++
id = "own-gherkin-reader"
title = "The engine reads and writes Gherkin with its own reader"
status = "accepted"
date = "2026-11-25"
supersedes = []
superseded_by = ""
+++

## Context

The engine must write a spec back after an edit, report problems with file and line, and refuse anything outside a small subset. It is also the component a cheaper model regenerates from its specs, so every dependency is surface it has to honour.

## Decision

The engine carries its own reader and writer for the subset and does not depend on a Gherkin library.

## Consequences

The subset is ours to define and to grow. Reading decides on characters and single words, never on phrases, so the repository's rule against control decisions that read prose holds without exceptions.

## Confirmation

`spec-engine.feature-format`, including the round-trip properties, and the dependency list checked by `spec-engine.contract/uses-no-other-crate-of-the-workspace`.
