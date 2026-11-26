@spec:spec-engine.scenario-ids
@decision:gherkin-syntax-without-a-runner
Feature: Scenario identifiers are stable
  A scenario is named "<spec>/<scenario>" from its tags, never from its
  title, so rewording a title breaks no test and no record.

  @id:title-change-keeps-id
  Scenario: Rewording a title keeps the identifier
    Given a spec "billing.refunds" whose scenario is tagged "@id:first-run"
    When its title changes from "First run" to "Initial run"
    Then the scenario is still named "billing.refunds/first-run"

  @id:duplicate-scenario-id-is-a-problem
  Scenario: Two scenarios of one spec with the same identifier
    Given a spec with two scenarios tagged "@id:first-run"
    When the spec is read
    Then reading fails with "duplicate_scenario_id" naming the file and the line of the second scenario

  @id:duplicate-spec-id-is-a-problem
  Scenario: Two files with the same spec identifier
    Given a project with two feature files tagged "@spec:billing.refunds"
    When the project is loaded
    Then loading fails with "duplicate_spec_id" naming the second file in path order
