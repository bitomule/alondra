@spec:spec-engine.boundary
@decision:component-boundaries
Feature: A component declares its boundary
  Each component has a `boundary.toml` in its own folder under the specs
  folder. It says which files the component owns, which of them may be
  deleted and written again, what else its result depends on and how its tests
  run. It delimits the component; it promises nothing by itself. Every promise
  of a boundary (its public surface, the components it may use, the data it
  must keep reading) is a scenario with a test, tagged "@boundary:", so that
  the feature files alone say everything a new implementation must honour.
  The feature files beside the boundary are the specs of that component.

  @id:reads-boundary
  Scenario: A complete boundary file
    Given "specs/billing/boundary.toml" with id "billing", owned files, regenerated files, inputs, a dependency on "accounts", data, fixtures, a surface snapshot and two suites
    When the project is loaded
    Then the component "billing" carries every value written in the file

  @id:spec-belongs-to-the-component-of-its-folder
  Scenario: Feature files beside a boundary belong to it
    Given "specs/billing/boundary.toml" and "specs/billing/refunds.feature"
    When the project is loaded
    Then the spec read from "specs/billing/refunds.feature" belongs to the component "billing"

  @id:spec-without-component-is-a-problem
  Scenario: A feature file in a folder with no boundary
    Given "specs/orphan/refunds.feature" and no "specs/orphan/boundary.toml"
    When the project is loaded
    Then loading fails with "spec_without_component" naming the feature file

  @id:folder-and-id-must-agree
  Scenario: A boundary whose id is not the name of its folder
    Given "specs/billing/boundary.toml" with id "payments"
    When the project is loaded
    Then loading fails with "component_id_mismatch" naming the boundary file and the line of its "id"

  @id:unknown-key-is-a-problem
  Scenario: A key the engine does not know
    Given a boundary file with a key "owner"
    When the project is loaded
    Then loading fails with "invalid_boundary" naming the file and the line of that key

  @id:ledger-inside-component-is-a-problem
  Scenario Outline: The record of validations can never be part of what it records
    Given a boundary whose "<list>" is "specs/**"
    When the project is loaded
    Then loading fails with "ledger_inside_component" naming the boundary file

    Examples:
      | list   |
      | owns   |
      | inputs |

  @id:ledger-reach-is-read-from-the-pattern
  Scenario Outline: Whether a pattern reaches the ledger is decided before any ledger file exists
    Given a repository with no ledger file and a boundary whose "<list>" is "<pattern>"
    When the project is loaded
    Then <answer>
    And a pattern of "owns" or "inputs" reaches the ledger when some path under "specs/ledger/" could match it: its segments are walked against "specs" and "ledger", a "**" reaches, a segment that matches goes on, one that does not cannot reach, a pattern that ends first cannot reach and a pattern with segments left after "ledger" reaches

    Examples:
      | list   | pattern                 | answer                                                                             |
      | owns   | specs/ledger/billing/** | loading fails with "ledger_inside_component" naming the boundary file and that line |
      | owns   | specs/*/billing/*.toml  | loading fails with "ledger_inside_component" naming the boundary file and that line |
      | inputs | **/*.toml               | loading fails with "ledger_inside_component" naming the boundary file and that line |
      | owns   | specs/ledger            | the project loads                                                                  |
      | owns   | specs/notes/**          | the project loads                                                                  |
      | data   | specs/ledger/**         | the project loads                                                                  |

  @id:oversized-boundary-is-a-problem
  Scenario: A boundary file beyond the size limit
    Given a boundary file of 65536 bytes, and another of 65537 bytes
    When a project with each is loaded
    Then the first loads and the second fails with "too_large" naming the boundary file

  @id:regenerated-files-must-be-owned
  Scenario: A component can only regenerate what it owns
    Given a boundary that owns "crates/billing/**" and regenerates "crates/ledger/src/**"
    When the project is loaded
    Then loading fails with "regenerates_outside_owns" naming the boundary file

  @id:unknown-dependency-is-a-problem
  Scenario: A dependency on a component that does not exist
    Given a boundary that depends on "accounts" in a project with no component "accounts"
    When the project is loaded
    Then loading fails with "unknown_component" naming the boundary file and the line of "depends_on"

  @id:dependency-cycle-is-a-problem
  Scenario: Two components that depend on each other
    Given "billing" depends on "accounts" and "accounts" depends on "billing"
    When the project is loaded
    Then loading fails with one "dependency_cycle" for the whole cycle
    And it names the boundary file of "accounts", the member whose id sorts first
    And its message lists "accounts, billing"

  @id:duplicate-suite-is-a-problem
  Scenario: Two suites with the same id
    Given a boundary with two suites whose id is "unit"
    When the project is loaded
    Then loading fails with "duplicate_suite_id" naming the boundary file and the line of the "id" of the second suite

  @id:suite-needs-a-command
  Scenario: A suite with nothing to run
    Given a boundary with a suite whose "run" list is empty
    When the project is loaded
    Then loading fails with "invalid_boundary" naming the boundary file and the line of that "run"

  @id:pattern-outside-repository-is-a-problem
  Scenario Outline: A pattern that leaves the repository
    Given a boundary whose "owns" list contains "<pattern>"
    When the project is loaded
    Then loading fails with "path_outside_repository" naming the boundary file and the line of that pattern

    Examples:
      | pattern        |
      | ../other/**    |
      | /usr/lib/**    |
      | crates/../../x |

  @id:promise-without-scenario-is-a-problem
  Scenario Outline: A boundary cannot promise what no scenario states
    Given a boundary that <declares> and no scenario of the component tagged "@boundary:<promise>"
    When the project is loaded
    Then loading fails with "boundary_without_scenario" naming the boundary file

    Examples:
      | declares                                  | promise      |
      | names a surface snapshot                  | surface      |
      | lists data it writes                      | data         |
      | exists, with or without dependencies      | dependencies |

  @id:promise-with-scenario-loads
  Scenario: A boundary whose promises are all scenarios
    Given a boundary with a surface snapshot, data and a dependency, and scenarios tagged "@boundary:surface", "@boundary:data" and "@boundary:dependencies"
    When the project is loaded
    Then the project loads and each of those scenarios is validated like any other
