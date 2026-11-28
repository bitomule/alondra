+++
id = "a-node-reserves-the-average-build-of-its-project"
title = "A node reserves the average build of its project, once, until its task ends"
status = "accepted"
date = "2026-11-28"
supersedes = []
superseded_by = ""
+++

## Context

The gate reserved for every new node the second highest peak its project ever took, and for a project measured fewer than three times the heaviest estimate of any project. Most nodes take little disk: a task that only runs a command, reads code or writes a document waited in the queue behind reservations sized for the worst build. Reservations also outlived their tasks: an admission counted until the next reading of the disk, each admission of the same task added one more estimate, and a task that kept handing over between stages kept adding them. On 28 November a high priority task that only had to run one command waited with 25.6 GB usable because the gate counted 18.6 GB for two nodes the samples measured at 11.6 GB.

## Decision

A node reserves the average of its project's kept peaks, never under 1 GB. A project measured fewer than three times reserves the larger of its own average and the average of every peak kept by the projects measured three times or more; with none, 8 GB. One task reserves at most one estimate however many times it is admitted between two readings, and nothing reserves for a task once it is over: cancelled, done or closing.

## Consequences

Tasks that do not build start sooner, and the queue only waits for space work still running may take. A build larger than the average is no longer covered by its reservation: the disk's floor, which holds the next turns of the nodes at work, is what stops several large builds from filling the disk, as it already was once a node had taken its estimate.

## Confirmation

`disk-gate.reservation` and `disk-gate.release`.
