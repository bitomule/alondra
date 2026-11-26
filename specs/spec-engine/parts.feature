@spec:spec-engine.parts
@decision:engine-reads-through-its-caller
Feature: A spec may keep diagrams and mockups next to its feature file
  A `@diagram:` or `@mockup:` tag on a feature or on a scenario names a file
  by its path from the folder of the feature file. A diagram is Mermaid text
  and a mockup is a PNG or JPEG image. The engine checks each file, puts its
  content in the fingerprints of what it illustrates and tells whoever lists
  or packs the specs which files they are.

  @id:reads-part-tags
  Scenario: A feature and a scenario cite their parts
    Given a feature tagged "@diagram:flow.mmd" with a scenario tagged "@mockup:screens/sheet.png" and "@mockup:empty.jpg"
    When the file is read
    Then the spec cites the diagram "flow.mmd"
    And the scenario cites the mockups "screens/sheet.png" and "empty.jpg" in that order

  @id:writes-part-tags-in-canonical-order
  Scenario: Where the tags of parts are written
    Given a spec with a decision, a diagram and a mockup, and a judged reserved scenario of a boundary promise with a diagram and a mockup
    When the spec is written
    Then the feature tags are one per line in the order spec, decision, diagram, mockup
    And the scenario tags are on one line in the order id, judged, evidence, boundary, reserved, diagram, mockup

  @id:rejects-part-outside-component
  Scenario Outline: A tag cannot name a file outside the folder of the component
    Given a scenario tagged "@diagram:<file>"
    When the file is read
    Then reading fails with "part_outside_component" naming the file and the line of the tag

    Examples:
      | file               |
      | /etc/flow.mmd      |
      | ../ledger/flow.mmd |
      | a/../flow.mmd      |
      | ./flow.mmd         |
      | a//flow.mmd        |
      | a\\flow.mmd        |

  @id:rejects-part-tag-that-cites-no-part
  Scenario Outline: A tag whose file cannot be that kind of part
    Given a scenario tagged "<tags>"
    When the file is read
    Then reading fails with "invalid_part" naming the file and the line of the tags

    Examples:
      | tags                                |
      | @diagram:flow.png                   |
      | @diagram:                           |
      | @diagram:.mmd                       |
      | @mockup:sheet.webp                  |
      | @mockup:sheet.mmd                   |
      | @mockup:sheet.png @mockup:sheet.png |
      | @diagram:flow.mmd @diagram:flow.mmd |

  @id:limits-parts-per-spec
  Scenario: A spec cites at most sixteen parts
    Given a spec whose feature and scenarios hold seventeen tags of diagrams and mockups
    When the file is read
    Then reading fails with "too_many_parts" naming the line of the seventeenth tag
    And sixteen tags read

  @id:missing-part-is-a-problem
  Scenario: A tag that names a file that is not there
    Given a scenario tagged "@mockup:sheet.png" and no such listed file
    When the project is read
    Then it fails with "missing_part" naming the feature file, the line of the tag and the file

  @id:unreadable-part-is-a-problem
  Scenario: A cited file that cannot be read
    Given a cited mockup that is listed and cannot be read
    When the project is read
    Then it fails with "unreadable_file" naming the mockup

  @id:rejects-oversized-part
  Scenario Outline: A part beyond its limit
    Given a cited <part> of <bytes> bytes
    When the project is read
    Then it fails with "too_large" naming the feature file, the line of the tag and the file
    And a <part> of one byte less is read

    Examples:
      | part    | bytes   |
      | diagram | 65537   |
      | mockup  | 2097153 |

  @id:diagram-is-of-a-drawable-kind
  Scenario: The first word of a diagram says what it is
    Given diagrams whose first word, after blank lines and lines that start with "%%", is "flowchart", "graph", "sequenceDiagram", "stateDiagram", "stateDiagram-v2" or "classDiagram"
    When the project is read
    Then each of them is read with its text

  @id:rejects-undrawable-diagram
  Scenario Outline: A diagram no client draws
    Given a cited diagram that <content>
    When the project is read
    Then it fails with "invalid_part" naming the feature file, the line of the tag and the file

    Examples:
      | content                 |
      | starts with "gantt"     |
      | starts with "flowcharts" |
      | holds only comments     |
      | is empty                |
      | is not valid UTF-8      |

  @id:reads-mockup-size
  Scenario Outline: A mockup is recognised by its bytes
    Given a cited mockup whose bytes are a <format> image 1170 wide and 2532 high
    When the project is read
    Then the mockup is read with that width and that height

    Examples:
      | format |
      | PNG    |
      | JPEG   |

  @id:rejects-mockup-that-is-no-image
  Scenario Outline: A mockup that is not a PNG or a JPEG
    Given a cited mockup whose bytes are <bytes>
    When the project is read
    Then it fails with "invalid_part" naming the feature file, the line of the tag and the file

    Examples:
      | bytes                            |
      | text                             |
      | a PNG signature and nothing else |
      | a PNG without its header chunk   |
      | a PNG of width 0                 |
      | a JPEG that ends before a frame  |

  @id:uncited-part-is-a-problem
  Scenario: A file of the folder that no tag cites
    Given files ending in ".mmd", ".png", ".jpg" and ".jpeg" in the folder of a component that no tag cites and no "@evidence:" pattern matches
    When the project is read
    Then it fails with "uncited_part" naming each of them
    And a file matched by an "@evidence:" pattern, a file of another extension and a file outside the folder of a component are not reported
    And no uncited file is reported for a folder with a feature file that cannot be read

  @id:listing-names-parts
  Scenario: The listing says which parts each spec and scenario cites
    Given a feature that cites a diagram and a scenario that cites a mockup
    When the project is listed
    Then the spec lists the diagram and the scenario lists the mockup
    And each part has its path from the root, its kind, its fingerprint and its bytes, and a mockup its width and height
