@spec:task-flow.continuation
@decision:a-continuation-owns-the-open-pull-requests
Feature: A task that continues another takes over its work
  A task the orchestrator creates to continue another takes the original's open
  worktrees, its checklist and its open pull requests in the same step. From then on the
  original never delivers, closes or works on them again, and it never gets a new agent
  while the task that continues it is not finished.

  @id:takes-open-prs
  Scenario: The continuation takes the open pull requests
    Given task "t-a" with its open pull request "example/repo#7" on branch "worktree-t-a"
    When the orchestrator creates task "t-b" with continues "t-a"
    Then "example/repo#7" belongs to "t-b"
    And "t-b" works in the worktree of "t-a" on branch "worktree-t-a"
    And the history of "example/repo#7" names "t-a" and then "t-b"

  @id:refused-moves-nothing
  Scenario: A refused continuation moves nothing
    Given task "t-a" with its open pull request "example/repo#7"
    When creating task "t-b" with continues "t-a" is refused because it belongs to another request
    Then "example/repo#7" belongs to "t-a"

  @id:delivery-open-is-settled-first
  Scenario: A delivery waiting for the user is settled first
    Given task "t-a" whose pull request "example/repo#7" waits for the user on its delivery card
    When the orchestrator creates task "t-b" with continues "t-a"
    Then the creation is refused with "delivery_open"
    And "example/repo#7" belongs to "t-a"

  @id:cancel-by-orchestrator
  Scenario: The orchestrator cancels the original
    Given task "t-b" continues task "t-a" and holds its pull request "example/repo#7"
    When the orchestrator cancels "t-a"
    Then "t-a" is cancelled
    And no card is raised in that step
    And "example/repo#7" is open and belongs to "t-b"

  @id:cancel-by-user
  Scenario: The user cancels the original
    Given task "t-b" continues task "t-a" and holds its pull request "example/repo#7"
    When the user cancels "t-a"
    Then "t-a" is cancelled
    And "example/repo#7" is open and belongs to "t-b" with no request to close it on GitHub

  @id:never-starts-again
  Scenario Outline: The original never gets a new agent
    Given task "t-b" continues task "t-a" and is not finished
    When <who> asks for a new agent for "t-a"
    Then it is refused with "task_continued" naming "t-b"
    And "t-a" has no agent

    Examples:
      | who                                     |
      | the orchestrator launching a node       |
      | the user asking for changes on its card |

  @id:running-original-cannot-take-it-back
  Scenario: A running original cannot take the pull request back
    Given task "t-b" continues task "t-a" and holds "example/repo#7" while "t-a" still runs
    When the agent of "t-a" registers "example/repo#7"
    Then it is refused with "pr_owned_by_other_task"
    And "example/repo#7" belongs to "t-b"

  @id:sweep-sees-the-continuation
  Scenario: The sweep of open pull requests counts the continuation as the branch's owner
    Given task "t-b" continues the cancelled task "t-a" in the worktree on branch "worktree-t-a" and is not finished
    When the sweep of open pull requests looks at a pull request on branch "worktree-t-a"
    Then the branch counts as having a live task
    And the pull request is neither adopted nor closed

  @id:older-continuation-moves-on-cancel
  Scenario: A continuation recorded by an older version
    Given task "t-b" created with continues "t-a" by version 0.3.72 while "example/repo#7" still belongs to "t-a"
    When the orchestrator or the user cancels "t-a"
    Then "example/repo#7" is open and belongs to "t-b"
    And "t-a" is cancelled with no card raised for it
