# The disk gate

How Alondra keeps room on the Mac's disk for the nodes it starts.

## What a node reserves
<!-- section: disk-gate.reservation-overview explains: disk-gate.reservation, decision:a-node-reserves-the-average-build-of-its-project -->

Before a node starts, the gate checks that the disk has room for what it may take on top of what
the nodes already at work may still take, plus a line of 5 GB. What a node may take is the average
of the peaks its project's earlier nodes took, measured from the Mac's own samples of the disk and
never under 1 GB. A project measured fewer than three times uses the average of the projects measured
enough, or its own if larger; before any project is measured enough, 8 GB. It is a minimum, not the
worst case: most nodes read, write and run commands without building. A node at work reserves its
estimate less what it already took, and a task counts one estimate however many times it starts,
resumes or hands over to another stage. When the estimates fall short and the disk drops under its
floor, the next turns of the nodes at work wait for room.

## When a reservation ends
<!-- section: disk-gate.release-overview explains: disk-gate.release -->

A reservation follows its task. Once the task is cancelled, done or closing, the gate stops counting
it at its next decision, without waiting for the next reading of the disk, so a task in the queue
never waits for space kept for work that no longer runs.

## Why a task waits for the disk
<!-- section: disk-gate.waiting-reason-overview explains: disk-gate.waiting-reason -->

When the disk has no room for the next task, its line in Tasks says how much is free, how much it
needs and which tasks hold the reserved space: the three that reserve most, by title and size, and
how many more. The orchestrator reads the whole list, and the daemon's log names every task by id.
