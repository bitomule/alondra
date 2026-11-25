+++
id = "settings-in-one-committed-file"
title = "A project turns specs on in one committed TOML file"
status = "accepted"
date = "2026-11-25"
supersedes = []
superseded_by = ""
+++

## Context

Specs, decisions and reference documents are optional per project, and a continuous integration run must see exactly what a developer's machine sees.

## Decision

`alondra.toml` at the root of the repository is the only source of these settings. No user setting or environment variable changes them. A missing file means everything is off; an unreadable file is a problem and never means off.

## Consequences

A project opts in with one reviewed change. TOML is used because the workspace already reads it and it has no implicit typing surprises.

## Confirmation

`spec-engine.settings`.
