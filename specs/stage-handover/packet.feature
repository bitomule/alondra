@spec:stage-handover.packet
@decision:a-stage-handover-references-the-plan-and-the-specs
Feature: What a new stage of a task starts from
  A task in stages changes agent from author to implementer, from implementer to validator
  and from validator to implementer. The new agent starts from a short packet Alondra writes
  for its role, never from the conversation of the agent before it. The packet holds what the
  role does, the worktree, branch and pull requests, and the validation that concerns the
  role, with every failed validation whole. Everything else goes by reference, to files the
  agent reads: the approved plan and the task's record (the original request, every message and
  answer of the user, the decisions on its cards and the earlier work it builds on) go to files
  of the task in Alondra's home, and the approved spec files go by their paths in the worktree,
  where Alondra already writes them.

  @id:plan-by-path
  Scenario: The approved plan goes to a file the packet names
    Given a task in stages "t-refund" whose approved plan is "# Refund part of an order" followed by the line "- [ ] Refund any amount up to what is left (billing.partial/part)"
    When the task hands over to its implementer
    Then the file "files/tasks/t-refund/approved-plan.md" in Alondra's home holds that approved plan exactly
    And the implementer's packet names the full path of that file and tells it to read the plan there before it starts
    And the line "- [ ] Refund any amount up to what is left (billing.partial/part)" is in that file and is not in the packet

  @id:phase-points-stay-inline
  Scenario: The points of a plan in phases that belong to the task stay in the packet
    Given a task in stages "t-phase" that implements points 2 and 3 of an approved plan in phases
    When the task hands over to its implementer
    Then the implementer's packet holds "Your part of it is points 2, 3"
    And the file "files/tasks/t-phase/approved-plan.md" in Alondra's home holds the whole plan

  @id:record-by-path
  Scenario: The task's record goes to a file the packet names, without the conversation
    Given a task in stages "t-refund" whose request is "Let customers get part of an order refunded"
    And the user told it "Use cents, not floats"
    And the user answered a card with "Refund to the original card"
    And its author wrote "I will start with the refund model" in its conversation
    When the task hands over to its implementer
    Then the file "files/tasks/t-refund/task-records.md" in Alondra's home holds "Let customers get part of an order refunded", "Use cents, not floats" and "Refund to the original card"
    And the implementer's packet names the full path of that file and tells it to search it and read only the part it needs
    And the texts "Use cents, not floats" and "I will start with the refund model" are not in the packet
    And the text "I will start with the refund model" is not in that file

  @id:specs-by-path
  Scenario: Approved specs written into the worktree go by their paths
    Given a task in stages whose approved specs "specs/billing/partial.feature" and "docs/decisions/refund-what-is-left.md" Alondra wrote into its worktree
    And "specs/billing/partial.feature" holds the scenario "Part of an order"
    When the task hands over to its implementer
    Then the implementer's packet lists "specs/billing/partial.feature" and "docs/decisions/refund-what-is-left.md"
    And it says to read them in the worktree, that a spec wins over any other text, and that a change of them goes through "request_spec_change"
    And the text "Part of an order" is not in the packet

  @id:uncommitted-specs-in-task-folder
  Scenario: Specs a project keeps out of the repository go to the task's folder
    Given a project that keeps its specs out of the repository
    And a task in stages "t-refund" whose approved spec is "specs/billing/partial.feature"
    When the task hands over to its implementer
    Then the file "files/tasks/t-refund/specs/specs/billing/partial.feature" in Alondra's home holds that spec exactly
    And the implementer's packet names the full path of that file and says not to commit it

  @id:failed-validations-whole
  Scenario: Every failed validation reaches the implementer whole
    Given a task in stages whose validator ended round 2 with "billing.partial/part" unmet because "The test refunds 30 EUR, not 10 EUR"
    And "billing/unit" could not be run because "The suite stopped at its time limit"
    And the validator's findings are "Two tests are skipped"
    When the task hands over to its implementer
    Then the implementer's packet holds "billing.partial/part", "The test refunds 30 EUR, not 10 EUR", "billing/unit", "The suite stopped at its time limit" and "Two tests are skipped"

  @id:large-task-hands-over
  Scenario: A task whose plan and specs outgrew the old packet hands over to its implementer
    Given a task in stages whose approved plan is 120,000 bytes long
    And whose approved spec files describe 172 scenarios in 90,000 bytes
    And whose validator sent the work back with 11 of its 172 validations unmet
    And the implementer's model has no context window in its account's catalog
    When the task hands over to its implementer
    Then the implementer starts on that model
    And no start problem is raised for the task
    And the implementer's packet holds each of the 11 unmet validations with the validator's reasoning

  @id:retry-after-size-refusal
  Scenario: Trying again a handover refused for size before this change starts the agent
    Given a task in stages whose handover to its implementer was refused three times because its context did not fit
    And the task waits on a start problem card that offers "Try again"
    When the user presses "Try again"
    Then the implementer starts
    And the start problem card is no longer open
