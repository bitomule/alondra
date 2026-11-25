+++
id = "gherkin-syntax-without-a-runner"
title = "Specs use Gherkin syntax and tests declare their scenario"
status = "accepted"
date = "2026-11-25"
supersedes = []
superseded_by = ""
+++

## Context

A spec has to be read by a program, edited by a person from an app and written well by a model. It also has to stay linked to the tests that verify it without forcing a project to change its test framework.

## Decision

A spec is a `.feature` file in a strict subset of Gherkin, used only as a syntax: nothing executes the steps. Tests stay in each project's own framework and declare the scenario they verify with a `spec: <spec>/<scenario>` comment above the test. A spec never lists its tests.

## Consequences

Scenarios get stable identifiers from tags. The link lives where it cannot rot unnoticed: a marker that points at nothing is a problem. Step text is prose for people and models, so a judge has to confirm that a test really checks its scenario.

## Confirmation

`spec-engine.feature-format` and `spec-engine.markers`.
