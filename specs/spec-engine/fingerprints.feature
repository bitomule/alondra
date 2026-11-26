@spec:spec-engine.fingerprints
@decision:engine-reads-through-its-caller
@decision:component-boundaries
Feature: A fingerprint says exactly what a validation looked at
  Every validation is recorded with the fingerprint of its inputs, written
  "sha256:" followed by 64 lowercase hexadecimal digits. When an input
  changes, the fingerprint changes and the validation is stale; when nothing
  it looked at changes, it stays valid. The engine only knows the files its
  caller lists, so a file outside that list can never change a result.

  @id:owned-file-changes-the-suite
  Scenario: Editing a file the component owns
    Given a component that owns "crates/billing/**" with a suite "unit"
    When the bytes of "crates/billing/src/lib.rs" change
    Then the fingerprint of the suite "unit" changes

  @id:input-file-changes-the-suite
  Scenario: Editing a file the component names as an input
    Given a component whose inputs are "Cargo.toml"
    When the bytes of "Cargo.toml" change
    Then the fingerprint of its suites changes

  @id:unrelated-file-keeps-the-suite
  Scenario: Editing a file the component neither owns nor names
    Given a component that owns "crates/billing/**"
    When the bytes of "crates/ledger/src/lib.rs" change
    Then the fingerprint of its suites stays the same

  @id:dependency-changes-the-suite
  Scenario: Editing a file of a component it depends on
    Given "billing" depends on "accounts", which depends on "money"
    When the bytes of a file owned by "money" change
    Then the fingerprint of the suites of "billing" changes

  @id:command-changes-the-suite
  Scenario: Changing how a suite runs
    Given a suite whose command is "bazelisk test //crates/billing:test"
    When the command gains the argument "--config=ci"
    Then the fingerprint of that suite changes and the fingerprint of the other suites stays the same

  @id:new-file-changes-the-suite
  Scenario: Adding or removing an owned file
    Given a component that owns "crates/billing/**"
    When a file "crates/billing/src/tax.rs" is added, and when it is removed again
    Then the fingerprint of its suites changes on the addition and returns to the first value on the removal

  @id:suite-test-file-changes-the-suite
  Scenario: Editing a test file of the suite that the component does not own
    Given a component that owns "crates/billing/**" with a suite "unit" whose tests are "crates/billing/tests/**/*.rs" and "tests/billing/*.rs"
    When the bytes of "tests/billing/extra.rs" change, and when that file is emptied
    Then the fingerprint of the suite "unit" changes each time

  @id:new-suite-test-file-changes-the-suite
  Scenario: Adding a test file of the suite that the component does not own
    Given a component that owns "crates/billing/**" with a suite "unit" whose tests are "crates/billing/tests/**/*.rs" and "tests/billing/*.rs"
    When a file "tests/billing/more.rs" is added, and when the pattern "tests/billing/*.rs" is replaced by "tests/billing/**/*.rs"
    Then the fingerprint of the suite "unit" changes each time

  @id:scenario-text-changes-only-that-scenario
  Scenario: Rewording a step
    Given a spec with scenarios "full" and "partial", each declared by a test
    When a step of "full" changes
    Then the adequacy fingerprint of "full" changes
    And the adequacy fingerprint of "partial" and the fingerprint of the suite stay the same

  @id:spec-header-changes-every-scenario
  Scenario: Changing what every scenario of a spec shares
    Given a spec with a description, a background and two scenarios
    When the description or a background step changes
    Then the adequacy fingerprint of both scenarios changes

  @id:test-file-changes-the-scenarios-it-declares
  Scenario: Editing a test file
    Given "full" declared in "tests/full.rs" and "partial" declared in "tests/partial.rs"
    When the bytes of "tests/full.rs" change
    Then the adequacy fingerprint of "full" changes and that of "partial" stays the same

  @id:evidence-changes-the-judgment
  Scenario: Replacing a capture a judged scenario points at
    Given a scenario tagged "@judged:capture @evidence:shots/refund*.png" and two files that match
    When the bytes of one of them change
    Then the judgment fingerprint of that scenario changes
    And editing an owned source file leaves it the same

  @id:judgment-without-evidence-follows-the-component
  Scenario Outline: A judged scenario with no pattern of committed evidence
    Given a scenario tagged "<tag>" in a component that owns "crates/billing/**"
    When the bytes of "crates/billing/src/lib.rs" change
    Then the judgment fingerprint of that scenario changes

    Examples:
      | tag             |
      | @judged         |
      | @judged:capture |
      | @judged:video   |

  @id:scenario-part-changes-its-validations
  Scenario: Changing a part of a scenario
    Given a spec with a judged scenario that cites a mockup, among scenarios declared by tests
    When the bytes of the mockup change
    Then the adequacy and judgment fingerprints of the scenario that cites it change
    And the fingerprints of the other scenarios and of the suite stay the same

  @id:feature-part-changes-every-adequacy
  Scenario: Changing a part of a feature
    Given a feature that cites a diagram, with a judged scenario among its scenarios
    When the bytes of the diagram change
    Then the adequacy fingerprint of every scenario of the spec changes
    And the judgment fingerprint and the fingerprint of the suite stay the same

  @id:a-part-changes-only-what-cites-it
  Scenario: Any part changes what it illustrates and nothing else
    Given any spec whose feature and scenarios cite any parts
    When the bytes of one part change
    Then exactly these fingerprints change: the adequacy of the scenarios that cite it, or of every scenario when the feature cites it, and the judgment of the judged scenarios that cite it

  @id:unlisted-file-never-counts
  Scenario: A file the caller does not list
    Given a component that owns "crates/billing/**"
    When a file "crates/billing/target/debug/out.bin" exists on disk and the caller does not list it
    Then every fingerprint is the same as without that file

  @id:listing-order-does-not-matter @reserved
  Scenario: The caller lists the same files in another order
    Given any project
    When its files are listed in a different order
    Then every fingerprint is the same

  @id:fingerprint-format
  Scenario: The written form of a fingerprint
    Given any project with a suite
    When its fingerprints are computed
    Then each one is "sha256:" followed by 64 lowercase hexadecimal digits
