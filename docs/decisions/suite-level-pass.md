+++
id = "suite-level-pass"
title = "A scenario passes with the suites that hold its tests"
status = "accepted"
date = "2026-11-25"
supersedes = []
superseded_by = ""
+++

## Context

Some test runners report a result per test and some only an exit status for the whole run.

## Decision

A suite is recorded as passed when its command ends well, with the fingerprint of the component at that moment. A scenario passes when every suite that holds one of its tests is recorded for the current fingerprint. When per-test results are given, the declared test itself must appear as passed.

## Consequences

The evidence is honest about its grain: a suite-level pass says the run that contains the test ended well. Unit, end-to-end and snapshot runners are all suites.

## Confirmation

`spec-engine.validation/scenario-passes-with-all-its-suites` and `spec-engine.validation/results-require-the-declared-test`.
