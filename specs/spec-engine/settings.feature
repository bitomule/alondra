@spec:spec-engine.settings
@decision:settings-in-one-committed-file
Feature: A project turns specs on in one committed file
  `alondra.toml` at the root of the repository is the only source of the
  settings. A project without it works as before; a file that cannot be read
  is a problem, never "off".

  @id:absent-file-turns-everything-off
  Scenario: A project without the file
    Given a project with no "alondra.toml"
    When the settings are read
    Then specs, decisions and reference are all disabled

  @id:defaults-apply
  Scenario: A file that only enables specs
    Given an "alondra.toml" with "format = 1" and "[specs] enabled = true"
    When the settings are read
    Then specs are enabled and committed, in "specs"
    And decisions are disabled, in "docs/decisions"
    And reference is disabled, in "docs/reference"

  @id:reads-every-setting
  Scenario: A file that sets everything
    Given an "alondra.toml" that sets specs to "product/specs" not committed, decisions to "adr" and reference to "handbook", all enabled
    When the settings are read
    Then every value is the one written in the file

  @id:unknown-key-is-a-problem
  Scenario: A key the engine does not know
    Given an "alondra.toml" whose "[specs]" table has "enable = true"
    When the settings are read
    Then reading fails with "invalid_settings" naming "alondra.toml" and the line of that key

  @id:broken-toml-is-a-problem
  Scenario: A file that is not valid TOML
    Given an "alondra.toml" with an unclosed table header
    When the settings are read
    Then reading fails with "invalid_settings" naming "alondra.toml"

  @id:folder-outside-repository-is-a-problem
  Scenario Outline: A folder that leaves the repository
    Given an "alondra.toml" whose <table> folder is "<folder>"
    When the settings are read
    Then reading fails with "path_outside_repository" naming "alondra.toml" and the line of that "dir"

    Examples:
      | table     | folder      |
      | specs     | ../specs    |
      | specs     | /etc/specs  |
      | specs     | specs/../.. |
      | decisions | ../adr      |

  @id:oversized-settings-are-a-problem
  Scenario: Settings beyond the size limit
    Given an "alondra.toml" of 65536 bytes, and another of 65537 bytes
    When the settings are read
    Then the first reads and the second fails with "too_large" naming "alondra.toml"

  @id:unsupported-format-is-a-problem
  Scenario: A format number newer than the engine
    Given an "alondra.toml" with "format = 2"
    When the settings are read
    Then reading fails with "unsupported_format" naming "alondra.toml"

  @id:disabled-project-loads-empty
  Scenario: A disabled project has nothing to validate
    Given a project with feature files and no "alondra.toml"
    When the project is loaded and reported
    Then the report is "clean" and lists nothing

  @id:markers-without-specs-when-not-committed
  Scenario: Specs kept outside the repository leave their markers behind
    Given a project whose settings say specs are not committed, with a component whose suite is recorded and no feature file
    And a test of that suite that carries a marker
    When the project is loaded and reported
    Then the marker is read and it is not a problem
    And the report is "clean"
