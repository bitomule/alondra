@spec:task-flow.specs-without-stages
@decision:one-agent-may-validate-its-own-specs
Feature: A task without stages proposes and validates its own specs
  In a project whose alondra.toml turns specs on, a task without stages may change specs.
  Its own agent proposes them, the user approves them on the card of that agent's plan, and
  the same agent validates its work against them before it delivers. It is the cheaper way
  for simpler work: the user's approval is what gives specs their weight, so they always
  reach the user before the work is delivered.

  @id:told-at-start
  Scenario: The agent is told the rule when it starts
    Given a project whose "alondra.toml" turns specs on
    And a task without stages
    When its first agent starts
    Then the agent's first input holds the section "## Specs in this project"
    And that section names "propose_specs", "ready_for_validation" and "record_validation"

  @id:nothing-added-without-specs
  Scenario: A project without specs adds nothing
    Given a project whose "alondra.toml" leaves specs off
    And a task without stages
    When its first agent starts
    Then the agent's first input equals its mission

  @id:proposes-without-stages
  Scenario: The agent of a task without stages proposes specs
    Given a task without stages in a project with specs
    When its agent calls "propose_specs" with level "light" and the feature file "specs/billing/refunds.feature"
    Then the task holds that proposal at level "light"
    And the card of the next plan the agent presents shows "specs/billing/refunds.feature" under "Specs"

  @id:refusal-names-the-way-out
  Scenario: The refusal of a task without stages says how to go on
    Given a task without stages whose branch changed "specs/billing/refunds.feature" without the user's approval
    When its agent presents its final result
    Then the result is refused with "specs_differ"
    And the refusal tells the agent to propose the file with "propose_specs" and present a plan

  @id:proposes-what-it-wrote
  Scenario: Specs already written are approved in the same task
    Given a task without stages whose delivery was refused with "specs_differ" for "specs/billing/refunds.feature"
    When its agent proposes that file as the worktree holds it and presents a plan
    And the user approves the plan
    Then the task's approved specs hold "specs/billing/refunds.feature" as proposed
    And the task has the same agent and no task was created for it

  @id:same-agent-goes-on
  Scenario: Approving the plan keeps the same agent
    Given a task without stages whose plan carries proposed specs
    When the user approves the plan
    Then the task's approved specs are exactly the proposed files
    And the agent that presented the plan is told to commit them once Alondra writes them
    And the task records one execution

  @id:validates-itself
  Scenario: The same agent validates its own work
    Given a task without stages with approved specs whose commit the engine lists as not validated
    When its agent calls "ready_for_validation"
    Then a validation round opens on that commit with that agent as its validator
    And "record_validation" from that agent is recorded in the round

  @id:unmet-comes-back-to-itself
  Scenario: Work it finds unmet comes back to the same agent
    Given a task without stages whose agent validates its own round
    When it records "billing.refunds/full" as "unmet" and calls "end_validation"
    Then the round ends unmet
    And the same agent is told to fix "billing.refunds/full" and call "ready_for_validation" again
    And the task records one execution

  @id:delivery-waits-for-validation
  Scenario: Delivery waits for the validation
    Given a task without stages with approved specs
    And the engine lists "billing.refunds/full" as not validated on the delivered commit
    When its agent presents its final result
    Then the result is refused with "validation_required"
    And the refusal tells the agent to call "ready_for_validation" and validate the work itself

  @id:delivery-says-same-agent
  Scenario: The delivery of a task without stages says who validated it
    Given a task without stages whose final delivery passed its validation
    When its delivery card is shown in English and in Spanish
    Then its section "Specs" says "Validated by the same agent that did the work"
    And in Spanish it says "Validado por el mismo agente que hizo el trabajo"

  @id:delivery-says-separate-agent
  Scenario: The delivery of a task in stages says a separate agent validated it
    Given a task in stages whose validator's delivery passed its validation
    When its delivery card is shown in English and in Spanish
    Then its section "Specs" says "Validated by a separate agent"
    And in Spanish it says "Validado por otro agente"

  @id:no-specs-delivered-as-before
  Scenario: A task without stages that changes no spec is delivered as before
    Given a task without stages in a project with specs whose branch changes no spec, boundary or decision
    And the task has no approved specs
    When its agent presents its final result
    Then the result is presented with no validation round

  @id:change-drafted-by-itself
  Scenario: A change to approved specs is drafted by the same agent
    Given a task without stages with approved specs
    When its agent calls "request_spec_change" with a reason
    Then the same agent is told to draft the change with "propose_amendment"
    And once drafted the user decides it on the change's card
    And the task records one execution

  @id:old-tasks-still-read
  Scenario: Tasks stored before this change still read
    Given a task without stages stored by version 0.3.72 with no proposed specs
    When Alondra reads it
    Then it reads as a task without stages, with no proposal and no validation round
