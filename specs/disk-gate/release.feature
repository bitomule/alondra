@spec:disk-gate.release
@decision:a-node-reserves-the-average-build-of-its-project
Feature: A task that ends releases its reservation at once
  What a task reserves on the disk follows the task, not the disk's readings: once it is over,
  whether cancelled, done or closing, nothing reserves for it at the gate's next decision, without
  waiting for the next reading of the disk. A task held in the queue never waits for space
  reserved for work that no longer runs.

  @id:ended-task-releases
  Scenario Outline: A task that ends stops reserving at the gate's next decision
    Given the project "app" kept peaks of 1 GB, 2 GB and 6 GB
    And the tasks "A" and "B" of "app" work and reserve 3.0 GB each
    And the disk was not read again since
    When "A" is <ending>
    Then the gate's next decision counts 3.0 GB reserved, all of it for "B"

    Examples:
      | ending                                  |
      | cancelled                               |
      | done                                    |
      | closing after its delivery was accepted |

  @id:held-command-task-starts
  Scenario: A task that only runs a command is not held by the reservations of a cancelled task
    Given the project "app" kept peaks of 1 GB, 2 GB and 6 GB
    And the disk has 25.6 GB usable and 21.8 GB free
    And the tasks "A" and "B" of "app" work and reserve 3.0 GB each
    And the task "C" of "app" was admitted three times since the last reading and then cancelled
    When the high priority task "D" of "app" waits to start
    Then the gate needs 14.0 GB for "D": 6.0 GB for "A" and "B", 3.0 GB for "D" and the 5 GB line
    And "D" starts
