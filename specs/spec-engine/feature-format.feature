@spec:spec-engine.feature-format
@decision:gherkin-syntax-without-a-runner
@decision:own-gherkin-reader
Feature: A spec is one feature file in a strict subset of Gherkin
  The engine reads a spec from the bytes of a `.feature` file and writes it
  back in one canonical form. Anything outside the subset is a problem that
  names the file and the line, never a guess. The subset is: tags, Feature,
  its description, Background, Scenario, Scenario Outline with Examples,
  steps (Given, When, Then, And, But), tables, doc strings and comments.

  @id:reads-feature-and-scenarios
  Scenario: A feature with two scenarios
    Given a file tagged "@spec:billing.refunds" with scenarios "@id:full" and "@id:partial"
    When the file is read
    Then the spec is "billing.refunds" with scenarios "full" and "partial" in that order
    And each scenario keeps its title, its line and its steps with their keywords

  @id:keeps-description
  Scenario: The prose under the feature title is kept
    Given a feature whose title is followed by two lines of description
    When the file is read
    Then the spec carries those two lines, trimmed, as its description

  @id:reads-decision-tags
  Scenario: A feature cites the decisions it follows
    Given a feature tagged "@decision:keep-ledger" and "@decision:one-file-per-spec"
    When the file is read
    Then the spec cites "keep-ledger" and "one-file-per-spec" in that order

  @id:reads-background
  Scenario: Background steps belong to the spec
    Given a feature with a Background of two steps before its scenarios
    When the file is read
    Then the spec carries the two background steps and the scenarios do not repeat them

  @id:reads-outline-with-examples
  Scenario: A Scenario Outline keeps its Examples table
    Given a Scenario Outline with an Examples table of a header row and two rows
    When the file is read
    Then the scenario carries the three rows with their cells trimmed

  @id:reads-doc-string-and-table
  Scenario: A step keeps its doc string and its table
    Given a step followed by a doc string of two lines and another step followed by a table
    When the file is read
    Then the first step carries the two lines joined by a line break
    And the second step carries the table rows

  @id:ignores-comments-and-blank-lines
  Scenario: Comments and blank lines carry no meaning
    Given a feature with lines starting with "#" and blank lines between its blocks
    When the file is read
    Then the spec equals the one read from the same file without them

  @id:reads-judgment-tags
  Scenario Outline: A scenario says which evidence its judgment needs
    Given a scenario tagged "<tags>"
    When the file is read
    Then the scenario is judged with evidence kind "<kind>" and evidence pattern "<pattern>"

    Examples:
      | tags                                      | kind    | pattern          |
      | @judged                                   | none    |                  |
      | @judged:capture                           | capture |                  |
      | @judged:capture @evidence:shots/**/*.png  | capture | shots/**/*.png   |
      | @judged:video                             | video   |                  |

  @id:rejects-unknown-evidence-kind
  Scenario: A kind of evidence the engine does not know
    Given a scenario tagged "@judged:audio"
    When the file is read
    Then reading fails with "unknown_tag" naming the file and the line of the tag

  @id:evidence-pattern-needs-captures
  Scenario Outline: A pattern of committed evidence only goes with captures
    Given a scenario tagged "<tags>"
    When the file is read
    Then reading fails with "evidence_without_capture" naming the file and the line of the tags

    Examples:
      | tags                                  |
      | @evidence:shots/a.png                 |
      | @judged @evidence:shots/a.png         |
      | @judged:video @evidence:clips/a.mov   |

  @id:reads-reserved-tag
  Scenario: A scenario kept away from implementers
    Given a scenario tagged "@reserved"
    When the file is read
    Then the scenario is reserved

  @id:reads-boundary-promise-tag
  Scenario Outline: A scenario states a promise of its component's boundary
    Given a scenario tagged "@boundary:<promise>"
    When the file is read
    Then the scenario carries the boundary promise "<promise>"

    Examples:
      | promise      |
      | surface      |
      | data         |
      | dependencies |

  @id:rejects-unknown-boundary-promise
  Scenario: A boundary promise the engine does not know
    Given a scenario tagged "@boundary:speed"
    When the file is read
    Then reading fails with "unknown_tag" naming the file and the line of the tag

  @id:rejects-unknown-tag
  Scenario: A tag the engine does not know is a problem
    Given a scenario tagged "@wip"
    When the file is read
    Then reading fails with "unknown_tag" naming the file and the line of the tag

  @id:rejects-unsupported-keyword
  Scenario Outline: A Gherkin keyword outside the subset is a problem
    Given a feature that contains a line "<line>" after its title
    When the file is read
    Then reading fails with "unsupported_syntax" naming the file and that line
    And the keywords refused this way are "Rule:", "Example:", "Scenarios:" and "Scenario" followed by any words other than "Outline" before the colon

    Examples:
      | line                              |
      | Rule: refunds need a receipt      |
      | Example: a full refund            |
      | Scenarios:                        |
      | Scenario Template: a full refund  |

  @id:description-may-hold-a-colon
  Scenario: A description line that starts with a word and a colon
    Given a feature whose description holds the lines "Note: refunds are rare" and "Why: the money must go back"
    When the file is read
    Then both lines are part of the description

  @id:rejects-step-without-keyword
  Scenario: A line inside a scenario that is not a step is a problem
    Given a scenario that contains the line "the customer is refunded"
    When the file is read
    Then reading fails with "unsupported_syntax" naming the file and that line

  @id:rejects-missing-spec-tag
  Scenario: A feature without its identifier is a problem
    Given a feature with no "@spec:" tag
    When the file is read
    Then reading fails with "missing_spec_tag" naming the file and the line of "Feature:"

  @id:rejects-scenario-without-id
  Scenario: A scenario without its identifier is a problem
    Given a scenario with no "@id:" tag
    When the file is read
    Then reading fails with "missing_scenario_id" naming the file and the line of "Scenario:"

  @id:rejects-invalid-identifier
  Scenario: Identifiers use lowercase letters, digits, dots and hyphens
    Given a feature tagged "@spec:Billing_Refunds"
    When the file is read
    Then reading fails with "invalid_identifier" naming the file and the line of the tag

  @id:rejects-oversized-input
  Scenario: A file beyond the limits is refused before it is parsed
    Given a file larger than 262144 bytes, and another with 201 scenarios
    When each file is read
    Then the first fails with "too_large" and the second with "too_many_scenarios"

  @id:scenario-limit-counts-scenarios
  Scenario: A doc string line that starts with "Scenario:" is text
    Given a file with 200 scenarios, one of which has a doc string of three lines that start with "Scenario:"
    When the file is read
    Then it reads, with 200 scenarios, and the doc string keeps those lines

  @id:doc-string-escapes-delimiter-lines
  Scenario: A doc string line that would close its doc string
    Given a doc string whose lines, without their indentation, are three quotes, the same three quotes each after a backslash, and that line escaped once more
    When the spec is written and the text is read again
    Then the doc string read equals the original
    And a line is written escaped when it is made only of backslashes and quotes and undoing the escape zero or more times gives three quotes
    And escaping writes each backslash as two backslashes and each quote as a backslash and a quote
    And undoing the escape reads each backslash followed by a backslash or a quote as that second character, and applies only to a line in which every character is part of such a pair
    And reading undoes the escape exactly once on such a line and keeps every other line as written

  @id:table-cells-escape-pipes-and-backslashes
  Scenario: A table cell that holds a pipe or a backslash
    Given a table whose cells hold a pipe, a backslash, both, and two words with spaces between them
    When the spec is written and the text is read again
    Then every cell read equals the original
    And a cell is written with each backslash as two backslashes and each pipe as a backslash and a pipe
    And reading a row takes a backslash followed by a pipe or a backslash as that second character, any other backslash as itself, and every other pipe as the end of a cell

  @id:rejects-invalid-utf8
  Scenario: A file that is not UTF-8 is a problem
    Given a file whose bytes are not valid UTF-8
    When the file is read
    Then reading fails with "invalid_utf8" naming the file

  @id:reports-every-problem
  Scenario: Every problem of a file is reported at once
    Given a feature with no "@spec:" tag and one scenario with no "@id:" tag
    When the file is read
    Then reading fails with both "missing_spec_tag" and "missing_scenario_id", ordered by line

  @id:writes-canonical-form
  Scenario: Writing a spec produces the canonical text
    Given a spec with two decisions, a description, a background, a judged scenario and an outline
    When the spec is written
    Then the text is exactly the canonical form
      """
      @spec:billing.refunds
      @decision:keep-ledger
      @decision:one-file-per-spec
      Feature: Refunds return the money
        A refund returns what the customer paid.
        It never exceeds the payment.

        Background:
          Given a paid order of 30 EUR

        @id:full @judged:capture @evidence:shots/**/*.png
        Scenario: Full refund
          When the order is refunded
          Then the customer receives 30 EUR
            | currency | amount |
            | EUR      | 30     |

        @id:partial
        Scenario Outline: Partial refund
          When <amount> EUR are refunded
          Then the customer receives <amount> EUR
            \"\"\"
            receipt
            \"\"\"

          Examples:
            | amount |
            | 10     |
      """

  @id:round-trips-any-spec
  Scenario: Reading what was written gives back the same spec
    Given any spec the engine can represent, including table cells with pipes, backslashes and spaces inside, doc string lines that are three quotes or their escaped forms, and description lines such as "Note: x"
    When it is written and the text is read again
    Then the spec read equals the original

  @id:canonical-text-is-stable
  Scenario: Writing what was read from canonical text changes nothing
    Given the canonical text of any spec
    When it is read and written again
    Then the text is identical byte for byte
