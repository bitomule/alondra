@spec:spec-engine.reference
@decision:three-levels-only-specs-rule
Feature: Reference prose is anchored to the specs it explains
  Reference documents are Markdown files in the reference folder. Each
  section starts with a "##" heading followed by a comment that gives the
  section an id and lists what it explains: specs by their id, boundaries as
  "boundary:<component>" and decisions as "decision:<id>". Prose can explain;
  it cannot hold a requirement of its own, and a spec always wins over it.

  @id:reads-sections
  Scenario: A document with two sections
    Given "docs/reference/billing.md" with two "##" sections, each followed by its comment
    When the project is loaded
    Then both sections are read with their id, their heading, their text and the list of what they explain

  @id:text-before-the-first-section-is-free
  Scenario: A title and an introduction
    Given a document that starts with a "#" title and a paragraph before its first "##" section
    When the project is loaded
    Then the project loads and that text belongs to no section

  @id:section-without-comment-is-a-problem
  Scenario: A section that says nothing about what it explains
    Given a "##" section with no comment on the line after its heading
    When the project is loaded
    Then loading fails with "section_explains_nothing" naming the document and the line of the heading

  @id:malformed-comment-is-a-problem
  Scenario Outline: A comment after the heading that is not a section comment
    Given a "##" section whose next line is "<comment>"
    When the project is loaded
    Then loading fails with "section_explains_nothing" naming the document and the line of the comment
    And any line after a heading that starts with "<!--" is read as a section comment, well formed or not

    Examples:
      | comment                                                      |
      | <!--section: billing.overview explains: billing.refunds -->  |
      | <!-- section billing.overview explains: billing.refunds -->  |
      | <!-- a note -->                                              |

  @id:oversized-document-is-a-problem
  Scenario: A reference document beyond the size limit
    Given a reference document of 262144 bytes, and another of 262145 bytes
    When a project with each is loaded
    Then the first loads and the second fails with "too_large" naming the document

  @id:empty-explains-is-a-problem
  Scenario: A comment with an empty list
    Given a section whose comment is "<!-- section: billing.overview explains: -->"
    When the project is loaded
    Then loading fails with "section_explains_nothing" naming the document and the line of the comment

  @id:unknown-target-is-a-problem
  Scenario Outline: A section that explains something that does not exist
    Given a section that explains "<target>" in a project without it
    When the project is loaded
    Then loading fails with "unknown_reference_target" naming the document and the line of the comment

    Examples:
      | target               |
      | billing.gone         |
      | boundary:gone        |
      | decision:gone        |

  @id:duplicate-section-is-a-problem
  Scenario: Two sections with the same id
    Given two sections with the id "billing.overview" in different documents
    When the project is loaded
    Then loading fails with "duplicate_section_id" naming the second document in path order

  @id:section-follows-what-it-explains
  Scenario Outline: A section goes stale when what it explains changes
    Given a section recorded as "met" that explains a spec, a boundary and a decision
    When <change> and the project is loaded again
    Then the section is "stale"

    Examples:
      | change                                   |
      | a step of that spec changes              |
      | the boundary file changes                |
      | the text of the decision changes         |
      | the text of the section itself changes   |

  @id:section-ignores-what-it-does-not-explain
  Scenario: Another spec changes
    Given a section recorded as "met" that explains the spec "billing.refunds"
    When a step of the spec "billing.invoices" changes and the project is loaded again
    Then the section stays "valid"

  @id:reference-off-reads-no-file
  Scenario: A project with reference turned off
    Given settings with reference disabled and Markdown files in "docs/reference"
    When the project is loaded
    Then none of them is read and the report lists no section
