@spec:spec-engine.decisions
@decision:three-levels-only-specs-rule
Feature: A lasting decision is a record the engine reads
  A decision lives in one Markdown file in the decisions folder. A header
  between two lines of "+++" carries its id, title, status, date and the
  decisions it replaces; the body has the sections Context, Decision,
  Consequences and Confirmation. Specs cite decisions, and only an accepted
  decision may be cited.

  @id:reads-decision
  Scenario: A complete decision file
    Given "docs/decisions/keep-ledger.md" with id "keep-ledger", a title, status "accepted", date "2026-11-25" and the four sections
    When the project is loaded
    Then the decision "keep-ledger" carries its title, status, date and the text of each section

  @id:file-name-and-id-must-agree
  Scenario: A decision whose id is not the name of its file
    Given "docs/decisions/keep-ledger.md" with id "drop-ledger"
    When the project is loaded
    Then loading fails with "decision_id_mismatch" naming the file

  @id:unknown-status-is-a-problem
  Scenario: A status outside proposed, accepted, rejected, superseded and deprecated
    Given a decision file with status "approved"
    When the project is loaded
    Then loading fails with "invalid_decision" naming the file and the line of the status

  @id:missing-section-is-a-problem
  Scenario Outline: Every decision explains itself
    Given a decision file with no "## <section>" section
    When the project is loaded
    Then loading fails with "missing_section" naming the file

    Examples:
      | section      |
      | Context      |
      | Decision     |
      | Consequences |
      | Confirmation |

  @id:missing-header-is-a-problem
  Scenario: A Markdown file with no header
    Given a file in the decisions folder that does not start with a "+++" line
    When the project is loaded
    Then loading fails with "invalid_decision" naming the file and line 1

  @id:citing-unknown-decision-is-a-problem
  Scenario: A spec that cites a decision nobody wrote
    Given a spec tagged "@decision:keep-ledger" in a project with no such decision
    When the project is loaded
    Then loading fails with "unknown_decision" naming the feature file and the line of the tag

  @id:citing-unaccepted-decision-is-a-problem
  Scenario Outline: A spec may only rest on an accepted decision
    Given a spec tagged "@decision:keep-ledger" and that decision with status "<status>"
    When the project is loaded
    Then loading fails with "decision_not_accepted" naming the feature file and the line of the tag

    Examples:
      | status     |
      | proposed   |
      | rejected   |
      | superseded |
      | deprecated |

  @id:replacement-links-both-ways
  Scenario: Replacing a decision marks the old one
    Given "per-test-ledger" with supersedes "keep-ledger", and "keep-ledger" with status "superseded" and superseded_by "per-test-ledger"
    When the project is loaded
    Then both decisions load and "keep-ledger" names "per-test-ledger" as its replacement

  @id:one-sided-replacement-is-a-problem
  Scenario: A replacement the old decision does not acknowledge
    Given "per-test-ledger" with status "accepted" and supersedes "keep-ledger", and "keep-ledger" still "accepted"
    When the project is loaded
    Then loading fails with "supersession_mismatch" naming the file of "keep-ledger"

  @id:superseded-decision-names-its-replacement
  Scenario Outline: A superseded decision whose replacement does not name it back
    Given "keep-ledger" with status "superseded" and superseded_by "<replacement>", and <other>
    When the project is loaded
    Then loading fails with "supersession_mismatch" naming the file of "keep-ledger" and the line of "superseded_by"

    Examples:
      | replacement     | other                                                    |
      |                 | no other decision                                        |
      | gone            | no decision "gone"                                       |
      | per-test-ledger | "per-test-ledger" accepted, with "keep-ledger" missing from its supersedes |

  @id:oversized-decision-is-a-problem
  Scenario: A decision file beyond the size limit
    Given a decision file of 262144 bytes, and another of 262145 bytes
    When a project with each is loaded
    Then the first loads and the second fails with "too_large" naming the decision file

  @id:decisions-off-reads-no-file
  Scenario: A project with decisions turned off
    Given settings with decisions disabled and a spec with no decision tag
    When the project is loaded
    Then no file of the decisions folder is read and the project has no decisions

  @id:citing-with-decisions-off-is-a-problem
  Scenario: A spec cites a decision in a project that keeps none
    Given settings with decisions disabled and a spec tagged "@decision:keep-ledger"
    When the project is loaded
    Then loading fails with "unknown_decision" naming the feature file and the line of the tag
