@spec:spec-engine.validation
@decision:engine-never-runs-anything
@decision:suite-level-pass
Feature: The report says what is validated and what is missing
  Validating compares what should be validated with the ledger and lists
  the difference. It reads; it never runs a command, starts an agent or
  writes a file. The report has a status: "clean" when nothing is missing,
  "pending" when something is, and "unreadable" when the project has
  problems. A caller turns them into the exit codes 0, 1 and 2.

  @id:clean-project
  Scenario: Everything recorded with the current fingerprints
    Given a project whose suite, adequacy and judgment entries hold the current fingerprints with verdict "met"
    When the project is reported
    Then the status is "clean" and every item is "valid"

  @id:kinds-of-suite-are-all-the-same
  Scenario: Unit tests, end-to-end tests and snapshot tests are all suites
    Given a component with three suites whose commands are a unit test runner, a browser end-to-end runner and a snapshot test runner
    When the project is reported
    Then each suite is an item of kind "suite" and each scenario depends on the suites whose test files declare it

  @id:items-of-a-scenario
  Scenario: What a scenario needs
    Given a scenario declared by a test, a judged scenario with no test and a judged scenario declared by a test
    When the project is reported
    Then the first has the items "passes" and "adequate"
    And the second has only "judged"
    And the third has "passes", "adequate" and "judged"

  @id:never-validated-is-missing
  Scenario: A project with an empty ledger
    Given a project with a suite and a scenario declared by a test, and no ledger file
    When the project is reported
    Then the status is "pending" and the suite, "passes" and "adequate" are "missing"

  @id:changed-inputs-are-stale
  Scenario: The ledger holds fingerprints of an earlier state
    Given a clean project
    When an owned source file changes and the project is loaded again
    Then the suite and "passes" are "stale" and "adequate" stays "valid"

  @id:unmet-verdict-is-reported
  Scenario: A verdict that says the scenario is not met
    Given a scenario whose adequacy entry holds the current fingerprint with verdict "unmet"
    When the project is reported
    Then its "adequate" item is "unmet" and the status is "pending"

  @id:uncovered-scenario-is-reported
  Scenario: A scenario nothing verifies
    Given a scenario with no test that declares it and no "@judged" tag
    When the project is reported
    Then its "passes" item is "uncovered" and the status is "pending"

  @id:scenario-passes-with-all-its-suites
  Scenario: A scenario declared by tests of two suites
    Given a scenario declared in test files of the suites "unit" and "contract", with only "unit" recorded
    When the project is reported
    Then its "passes" item is "missing"

  @id:problems-make-it-unreadable
  Scenario: A project that does not load
    Given a project whose loading fails with three problems, two of them in the same file
    When a report is built from them, handed over in any order
    Then the status is "unreadable" and there are no items
    And the problems are listed in order of path, and of line within a path

  @id:results-require-the-declared-test
  Scenario Outline: With test results, the declared test itself must have passed
    Given a clean project whose scenario is declared by the test "refunds_in_full"
    When it is reported with results where that test <outcome>
    Then its "passes" item is "<state>"
    And a case is that test when its name is the test name, or ends in "::" or "." and the test name; the test passed when at least one case is that test and every case that is that test passed

    Examples:
      | outcome                                                                        | state  |
      | passed as "refunds_in_full"                                                    | valid  |
      | passed as "billing::tests::refunds_in_full"                                    | valid  |
      | passed as "BillingTests.refunds_in_full"                                       | valid  |
      | failed                                                                         | failed |
      | was skipped                                                                    | failed |
      | does not appear                                                                | failed |
      | failed as "billing::refunds_in_full" and passed as "ledger::refunds_in_full"   | failed |

  @id:results-need-a-test-name
  Scenario: A marker above no test the engine can name
    Given a clean project whose scenario "full" is declared by a marker with no test name in the eight lines below it
    When it is reported with results, and when it is reported without them
    Then its "passes" item is "failed" with results and "valid" without

  @id:unreadable-listed-file-is-a-problem
  Scenario Outline: A listed file the caller cannot read
    Given a project that lists "<file>" and whose caller reads nothing for it
    When the project is loaded
    Then loading fails with "unreadable_file" naming that file

    Examples:
      | file                                    |
      | alondra.toml                            |
      | specs/billing/boundary.toml             |
      | specs/billing/refunds.feature           |
      | docs/decisions/keep-ledger.md           |
      | docs/reference/billing.md               |
      | specs/ledger/billing/suites/unit.toml   |
      | crates/billing/tests/full.rs            |

  @id:oversized-results-are-a-problem
  Scenario: Test results beyond the size limit
    Given JUnit documents of 16777216 bytes and of 16777217 bytes
    When the results are read
    Then the first reads and the second fails with "too_large"

  @id:reads-junit-results
  Scenario: Test results come as JUnit XML
    Given a JUnit document with a passed case, a failed case, a case with an error and a skipped case
    When the results are read
    Then only the first case counts as passed

  @id:unreadable-results-are-a-problem
  Scenario: Results that are not JUnit XML
    Given bytes that are not well-formed XML
    When the results are read
    Then reading fails with "invalid_results"

  @id:report-as-json
  Scenario: The report in its machine-readable form
    Given a project with one suite "unit" that is "valid" and one scenario "billing.refunds/full" whose adequacy is "missing"
    When the report is written as JSON
    Then the text is exactly
      """
      {
        "format": 1,
        "status": "pending",
        "items": [
          {
            "subject": "billing.refunds/full",
            "component": "billing",
            "kind": "adequate",
            "state": "missing"
          },
          {
            "subject": "billing.refunds/full",
            "component": "billing",
            "kind": "passes",
            "state": "valid"
          },
          {
            "subject": "billing/unit",
            "component": "billing",
            "kind": "suite",
            "state": "valid"
          }
        ],
        "leftover": [],
        "problems": []
      }
      """

  @id:problem-as-json
  Scenario: A problem in its machine-readable form
    Given a report built from the problem "missing_spec_tag" at line 3 of "specs/billing/refunds.feature"
    When the report is written as JSON
    Then its "problems" list holds an object with "code", "path", "line" and "message", in that order

  @id:report-is-deterministic @reserved
  Scenario: The same project always gives the same report
    Given any project
    When it is loaded and reported twice, with its files listed in different orders
    Then both reports are byte for byte the same JSON

  @id:reads-each-file-at-most-once
  Scenario: Loading and reporting never read a file twice
    Given a project with 40 scenarios, 12 test files and 300 owned files
    When it is loaded and reported
    Then no path is read more than once
    And no file outside the settings, the specs, decisions and reference folders, the owned files, the inputs and the evidence is read at all
