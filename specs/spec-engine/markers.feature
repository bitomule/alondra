@spec:spec-engine.markers
@decision:gherkin-syntax-without-a-runner
Feature: A test declares the scenario it verifies
  The link between a scenario and its tests is written in the test: a
  comment "spec: <spec>/<scenario>" on the lines right above it. A spec never
  lists its tests. The engine looks for markers only in the files a suite
  names as its tests.

  @id:finds-markers-in-comments
  Scenario Outline: A marker in a comment of the test language
    Given a suite test file with the line "<line>" above a test
    When the project is loaded
    Then that file declares the scenario "billing.refunds/full" at that line

    Examples:
      | line                              |
      | // spec: billing.refunds/full     |
      | # spec: billing.refunds/full      |
      |     /// spec: billing.refunds/full |

  @id:names-the-test-under-the-marker
  Scenario Outline: The marker belongs to the next test function
    Given a marker followed, after attribute lines, by "<declaration>"
    When the project is loaded
    Then the marker names the test "<name>"

    Examples:
      | declaration                     | name            |
      | fn refunds_in_full() {          | refunds_in_full |
      | async fn refunds_in_full() {    | refunds_in_full |
      | func refundsInFull() throws {   | refundsInFull   |
      | def test_refunds_in_full(self): | test_refunds_in_full |
      | test('refunds in full', async ({ page }) => { | refunds in full |
      | it("refunds in full", () => {   | refunds in full |

  @id:one-test-declares-several-scenarios
  Scenario: Several markers above one test
    Given two marker lines, for "billing.refunds/full" and "billing.refunds/partial", above one test
    When the project is loaded
    Then both scenarios are declared by that test

  @id:several-tests-declare-one-scenario
  Scenario: One scenario verified by tests in two files
    Given markers for "billing.refunds/full" in two suite test files
    When the project is loaded
    Then the scenario is declared by both files, in path order

  @id:orphan-marker-is-a-problem
  Scenario: A marker for a scenario that does not exist
    Given a suite test file with a marker for "billing.refunds/gone" and no such scenario
    When the project is loaded
    Then loading fails with "orphan_marker" naming the test file and the line of the marker

  @id:only-suite-tests-are-searched
  Scenario: A marker outside the files of any suite
    Given a marker for a scenario that does not exist, in a file no suite names as its tests
    When the project is loaded
    Then the project loads and that file is never read

  @id:malformed-marker-is-a-problem
  Scenario: A marker that does not name a spec and a scenario
    Given a suite test file with the comment "// spec: billing.refunds"
    When the project is loaded
    Then loading fails with "invalid_marker" naming the test file and that line

  @id:test-of-another-component-is-a-problem
  Scenario: A test declares a scenario of another component
    Given a marker for a scenario of "accounts" in a test file of a suite of "billing"
    When the project is loaded
    Then loading fails with "marker_outside_component" naming the test file and the line of the marker
