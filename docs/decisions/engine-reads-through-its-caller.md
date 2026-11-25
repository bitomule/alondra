+++
id = "engine-reads-through-its-caller"
title = "The engine only sees what its caller hands it"
status = "accepted"
date = "2026-11-25"
supersedes = []
superseded_by = ""
+++

## Context

The same validation has to give the same answer on a developer's machine, in the daemon and in continuous integration, and a build output or any other file git ignores must never change it.

## Decision

The engine never touches the disk, the network, the clock or another program. Its caller lists the files of the repository and reads them on request. Fingerprints are SHA-256 over those bytes.

## Consequences

The engine is deterministic and easy to test with files in memory. Callers decide which files those are; the command asks git for the files it tracks and the new ones it does not ignore, which is what a commit of the whole working tree would hold.

## Confirmation

`spec-engine.fingerprints` and `spec-engine.contract/reads-only-through-its-caller`.
