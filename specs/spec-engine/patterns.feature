@spec:spec-engine.patterns
@decision:engine-reads-through-its-caller
Feature: Path patterns choose files inside the repository
  Boundaries, suites and evidence name files with patterns. A pattern is a
  path relative to the root of the repository, with "/" between segments,
  where "*" stands for any run of characters inside one segment and "**"
  for any number of whole segments, none included.

  @id:star-stays-in-one-segment
  Scenario Outline: A single star
    Given the pattern "crates/*/Cargo.toml"
    When it is matched against "<path>"
    Then the answer is <answer>

    Examples:
      | path                          | answer |
      | crates/billing/Cargo.toml     | yes    |
      | crates/billing/sub/Cargo.toml | no     |
      | crates/Cargo.toml             | no     |

  @id:double-star-crosses-segments
  Scenario Outline: A double star
    Given the pattern "crates/billing/**/*.rs"
    When it is matched against "<path>"
    Then the answer is <answer>

    Examples:
      | path                             | answer |
      | crates/billing/lib.rs            | yes    |
      | crates/billing/src/tax/rates.rs  | yes    |
      | crates/billing/src/tax/rates.txt | no     |
      | crates/ledger/src/lib.rs         | no     |

  @id:trailing-double-star-takes-everything-below
  Scenario Outline: A pattern that ends in a double star
    Given the pattern "specs/**"
    When it is matched against "<path>"
    Then the answer is <answer>

    Examples:
      | path                        | answer |
      | specs/billing/boundary.toml | yes    |
      | specs/a                     | yes    |
      | specs                       | no     |
      | specsheet/a                 | no     |

  @id:literal-pattern-matches-itself
  Scenario: A pattern with no star
    Given the pattern "Cargo.toml"
    When it is matched against "Cargo.toml" and against "crates/billing/Cargo.toml"
    Then only the first matches

  @id:matching-is-case-sensitive
  Scenario: Upper and lower case are different
    Given the pattern "docs/*.md"
    When it is matched against "docs/README.MD"
    Then it does not match

  @id:repeated-double-stars-are-one
  Scenario: Consecutive double stars
    Given the patterns "a/**/**/x" and "specs/**/**"
    When they are matched against "a/x", "a/b/c/x", "specs" and "specs/a"
    Then each answers what "a/**/x" and "specs/**" answer: "a/x", "a/b/c/x" and "specs/a" match, "specs" does not

  @id:matching-time-stays-polynomial @reserved
  Scenario: Patterns full of stars still answer at once
    Given the pattern made of 30 segments "**" and then "x", and the pattern "*a*a*a*a*a*a*a*a*a*a*b"
    When the first is matched against a path of 40 segments "a", and the second against a segment of 60 letters "a"
    Then both answer "no", each in less than ten seconds
    And a match never tries the same place of the pattern against the same place of the path twice
