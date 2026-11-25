+++
id = "component-boundaries"
title = "A component has a boundary, and every promise of it is a scenario"
status = "accepted"
date = "2026-11-25"
supersedes = []
superseded_by = ""
+++

## Context

Code can only be deleted and written again if something says where a component ends, and if everything a new implementation must honour is stated in a form a test verifies.

## Decision

Each component has a `boundary.toml` that delimits it: the files it owns, the ones that may be regenerated, the inputs and components its result depends on and how its tests run. The boundary promises nothing by itself: its public surface, its dependencies and the stored data it must keep reading are scenarios tagged `@boundary:`, each with a test. A boundary that declares a promise with no scenario is a problem.

## Consequences

The feature files alone tell an implementer everything that is verified. The component is the unit of regeneration and the scope of a fingerprint.

## Confirmation

`spec-engine.boundary` and `spec-engine.contract`.
