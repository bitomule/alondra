@spec:spec-engine.ledger
@decision:ledger-holds-fingerprints-only
Feature: The ledger records what was validated, and nothing else
  The ledger is the folder "ledger" inside the specs folder. It keeps one
  small TOML file per suite, per scenario and per reference section, so two branches only collide when they validate the same thing.
  A file holds fingerprints and verdicts: no dates, no names, no free text.
  The engine never writes files; it hands the caller the path and the bytes.

  @id:suite-entry
  Scenario: The entry of a suite that passed
    Given a component "billing" with a suite "unit" in a project whose specs folder is "specs"
    When the suite is recorded as passed
    Then the engine hands back the path "specs/ledger/billing/suites/unit.toml"
    And its bytes are one line, inputs = "<the fingerprint of the suite>", ending in a line break

  @id:scenario-entry
  Scenario: The entry of a scenario
    Given the scenario "billing.refunds/full" of the component "billing"
    When its adequacy is recorded as "met"
    Then the engine hands back the path "specs/ledger/billing/scenarios/billing.refunds/full.toml"
    And its bytes are exactly
      """
      [adequate]
      inputs = "<the adequacy fingerprint>"
      verdict = "met"
      """

  @id:second-verdict-keeps-the-first
  Scenario: Recording the judgment of a scenario that already has its adequacy
    Given a scenario tagged "@judged:capture" whose entry already holds "[adequate]"
    When its judgment is recorded as "met" with the evidence bytes of "refund.png"
    Then the new bytes keep "[adequate]" unchanged and add "[judged]" with inputs, verdict and an "evidence" list holding the fingerprint of those bytes

  @id:judgment-needs-its-evidence
  Scenario Outline: A judgment that needs evidence cannot be recorded without it
    Given a scenario tagged "<tag>"
    When its judgment is recorded with <evidence>
    Then the engine answers "<answer>"

    Examples:
      | tag             | evidence                    | answer            |
      | @judged:capture | no evidence                 | evidence_required |
      | @judged:capture | a file named "refund.mov"   | evidence_required |
      | @judged:capture | a file named "refund.png"   | recorded          |
      | @judged:video   | a file named "refund.png"   | evidence_required |
      | @judged:video   | a file named "refund.mp4"   | recorded          |
      | @judged         | no evidence                 | recorded          |

  @id:evidence-kinds-by-extension
  Scenario Outline: Which files are captures and which are videos
    Given an evidence file named "<name>"
    When its kind is asked for
    Then it is "<kind>"

    Examples:
      | name        | kind    |
      | a.png       | capture |
      | a.jpg       | capture |
      | a.jpeg      | capture |
      | a.webp      | capture |
      | a.mov       | video   |
      | a.mp4       | video   |
      | a.webm      | video   |
      | a.txt       | none    |
      | a.PNG       | none    |

  @id:unmet-verdict-is-recorded
  Scenario: A verdict that the scenario is not met
    Given the scenario "billing.refunds/full"
    When its adequacy is recorded as "unmet"
    Then the entry holds verdict = "unmet" and the report shows the scenario as "unmet"

  @id:reference-entry
  Scenario: The entry of a reference section
    Given a reference section "billing.overview"
    When it is recorded as "met"
    Then the engine hands back the path "specs/ledger/reference/billing.overview.toml" with its fingerprint and verdict

  @id:reads-entries-written-by-format-1 @boundary:data
  Scenario: Entries written by an earlier build still read
    Given the ledger files kept as fixtures of format 1
    When a project that contains them is loaded
    Then every entry is read with the fingerprint and verdict written in it

  @id:unreadable-entry-is-a-problem
  Scenario Outline: A ledger file the engine cannot trust
    Given a ledger file that <defect>
    When the project is loaded
    Then loading fails with "invalid_ledger" naming that file

    Examples:
      | defect                               |
      | is not valid TOML                    |
      | has a key "note"                     |
      | has a verdict "maybe"                |
      | has inputs that are not a fingerprint |

  @id:judged-entry-needs-its-evidence
  Scenario Outline: A judged entry written by hand without evidence
    Given a scenario tagged "<tag>" and a ledger file whose "[judged]" table holds its current fingerprint, verdict "met" and no "evidence"
    When the project is loaded
    Then <outcome>

    Examples:
      | tag             | outcome                                                        |
      | @judged:capture | loading fails with "evidence_required" naming that ledger file |
      | @judged:video   | loading fails with "evidence_required" naming that ledger file |
      | @judged         | the project loads and the judgment is "valid"                  |

  @id:oversized-entry-is-a-problem
  Scenario: A ledger file beyond the size limit
    Given a ledger file of 65536 bytes, and another of 65537 bytes
    When a project that contains each is loaded
    Then the first loads and the second fails with "too_large" naming that ledger file

  @id:entry-of-nothing-is-ignored
  Scenario: An entry left behind by a scenario that no longer exists
    Given a ledger file for the scenario "billing.refunds/gone" and no such scenario
    When the project is loaded and reported
    Then the project loads and the report lists that path under "leftover"

  @id:recording-unknown-subject-is-a-problem
  Scenario: Recording a verdict for something that does not exist
    Given a project with no scenario "billing.refunds/gone"
    When its adequacy is recorded
    Then the engine answers "unknown_subject" and hands back nothing

  @id:judgment-of-plain-scenario-is-a-problem
  Scenario: Recording a judgment for a scenario that is not judged
    Given a scenario with no "@judged" tag
    When its judgment is recorded
    Then the engine answers "not_judged" and hands back nothing
