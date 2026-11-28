@spec:disk-gate.reservation
@decision:a-node-reserves-the-average-build-of-its-project
Feature: What a node reserves on the disk
  Before a node starts, the gate reserves on the Mac's own disk what a node of its project
  takes on average at its peak, measured from the nodes of that project that ran before. It is
  a minimum, not the worst case: most nodes take little, and the disk's floor still holds the
  next turns of the nodes at work when the estimates fall short. A node at work reserves that
  estimate less what it already took, and one task never reserves more than one estimate.

  @id:average-of-its-project
  Scenario: A project measured three times or more reserves the average of its peaks
    Given the project "app" kept peaks of 1 GB, 2 GB and 6 GB
    When a task of "app" waits to start
    Then the gate reserves 3.0 GB for it

  @id:never-under-one-gigabyte
  Scenario: The average is never under 1 GB
    Given the project "app" kept peaks of 0.1 GB, 0.2 GB and 0.3 GB
    When a task of "app" waits to start
    Then the gate reserves 1.0 GB for it

  @id:project-measured-too-few-times
  Scenario: A project measured fewer than three times reserves the average of the measured ones, or its own if larger
    Given the project "app" kept peaks of 1 GB, 2 GB and 6 GB
    And the project "web" kept peaks of 2 GB, 2 GB and 2 GB
    And the project "new" kept one peak of 0.5 GB
    And the project "big" kept one peak of 9 GB
    When a task of "new" and a task of "big" wait to start
    Then the gate reserves 2.5 GB for the task of "new"
    And the gate reserves 9.0 GB for the task of "big"

  @id:nothing-measured-yet
  Scenario: With no project measured three times the gate reserves 8 GB
    Given no project kept three peaks
    When a task of "app" waits to start
    Then the gate reserves 8.0 GB for it

  @id:one-estimate-per-task
  Scenario: A task admitted several times between two disk readings reserves one estimate
    Given the project "app" kept peaks of 1 GB, 2 GB and 6 GB
    And the disk was read once
    When the gate admits the task "A" of "app" three times before the next reading, for its start, a resume and a handover to another stage
    Then the gate counts 3.0 GB reserved for "A"
