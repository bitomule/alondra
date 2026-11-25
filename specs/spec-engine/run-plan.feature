@spec:spec-engine.run-plan
@decision:engine-never-runs-anything
@decision:suite-level-pass
Feature: The engine plans what to run and records what passed
  Running a suite is the caller's job. The engine says which commands a
  component needs, as lists of arguments, and, once the caller reports that
  a suite finished well, hands back the ledger entry to write.

  @id:plans-every-suite
  Scenario: The plan of a component
    Given a component "billing" with the suites "unit" and "contract"
    When its plan is asked for
    Then the plan lists both suites in the order of the boundary file, each with its arguments and its time limit in minutes

  @id:plans-one-suite
  Scenario: The plan of one suite
    Given a component "billing" with the suites "unit" and "contract"
    When the plan of the suite "contract" is asked for
    Then the plan lists only "contract"

  @id:default-time-limit
  Scenario: A suite that sets no time limit
    Given a suite with no "timeout_minutes"
    When its plan is asked for
    Then its time limit is 30 minutes

  @id:unknown-component-or-suite
  Scenario Outline: Asking for what does not exist
    Given a project with a component "billing" and a suite "unit"
    When the plan of <target> is asked for
    Then the engine answers "<code>"

    Examples:
      | target                                | code              |
      | the component "accounts"                | unknown_component |
      | the suite "smoke" of "billing"        | unknown_suite     |

  @id:passed-suite-is-recorded
  Scenario: A suite that finished well
    Given a project with an empty ledger
    When the suite "unit" of "billing" is recorded as passed and the entry is added to the project files
    Then the reloaded project reports that suite as "valid"

  @id:recording-never-touches-other-entries
  Scenario: Recording one suite
    Given a project with two suites
    When one is recorded as passed
    Then the engine hands back exactly one path, the entry of that suite
