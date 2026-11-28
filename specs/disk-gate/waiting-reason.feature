@spec:disk-gate.waiting-reason
Feature: The waiting reason names the tasks that hold the disk
  If the disk has no room for the next task, its waiting reason says which tasks hold the
  reserved space and how much each one, so the user can tell what to stop or wait for. The
  task's line in Tasks and its detail name the three that reserve most, by title, with "and N
  more" for the rest; the orchestrator and the log get every one.

  Background:
    Given the tasks "Alpha", "Beta", "Gamma" and "Delta" work and reserve 4.0 GB, 3.0 GB, 2.0 GB and 1.0 GB
    And the disk has 12.0 GB usable
    And the task "Echo" waits to start, alone in the queue, and needs 18.0 GB

  @id:line-names-three
  Scenario: The task's line names the three that reserve most, in English
    When the user reads "Echo" in Tasks in English
    Then its waiting line reads "1st of 1 · Disk · 12.0 GB free of 18.0 GB needed · reserved by “Alpha” 4.0 GB, “Beta” 3.0 GB, “Gamma” 2.0 GB and 1 more"

  @id:line-names-three-spanish
  Scenario: The task's line names the three that reserve most, in Spanish
    When the user reads "Echo" in Tasks in Spanish
    Then its waiting line reads "1.º de 1 · Disco · 12,0 GB libres de 18,0 GB necesarios · reservado por «Alpha» 4,0 GB, «Beta» 3,0 GB, «Gamma» 2,0 GB y 1 más"

  @id:orchestrator-gets-every-one
  Scenario: The orchestrator reads every task that holds the disk
    When the orchestrator reads where "Echo" waits
    Then the text names “Alpha” 4.0 GB, “Beta” 3.0 GB, “Gamma” 2.0 GB and “Delta” 1.0 GB, in that order

  @id:log-gets-every-one
  Scenario: The gate's log line names every task that holds the disk
    When the gate logs why it holds the queue
    Then the line names the ids of the four tasks with 4.00 GB, 3.00 GB, 2.00 GB and 1.00 GB

  @id:nothing-else-reserves
  Scenario: With nothing else reserving, the reason names no task
    Given no other task works
    And the disk has 4.0 GB usable
    When the user reads "Echo" in Tasks in English
    Then its waiting line reads "1st of 1 · Disk · 4.0 GB free of 5.0 GB needed"
