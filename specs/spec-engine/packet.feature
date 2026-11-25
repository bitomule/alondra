@spec:spec-engine.packet
@decision:three-levels-only-specs-rule
@decision:component-boundaries
Feature: The packet is everything a fresh implementer receives
  The packet of a component is one text, built only from committed files,
  that a new agent can work from with no earlier conversation. The specs in
  it are the source of truth; everything else in it is context.

  @id:states-that-specs-win
  Scenario: The rule every packet opens with
    Given any component
    When its packet is built
    Then its second paragraph is exactly "Specs are the only source of truth. Where any other text disagrees with a spec, the spec wins."

  @id:says-what-may-be-written
  Scenario: The files an implementer may write and the ones it may not
    Given a component that regenerates "crates/billing/src/**"
    When its packet is built
    Then the section "What you may write" lists "crates/billing/src/**"
    And the section "What you must not change" names the specs, the tests, the boundary, the fixtures and the ledger

  @id:carries-the-specs-in-canonical-form
  Scenario: Every spec of the component, as written
    Given a component with two specs
    When its packet is built
    Then the section "Specs" holds the canonical text of both, in order of their id

  @id:leaves-reserved-scenarios-out
  Scenario: A reserved scenario never reaches an implementer
    Given a spec with a scenario tagged "@reserved"
    When the packet of its component is built
    Then neither the title nor the steps of that scenario appear in the packet

  @id:carries-the-boundary
  Scenario: The boundary in the packet
    Given a component with inputs, a dependency, data, fixtures, a surface snapshot and two suites
    When its packet is built
    Then the section "Boundary" lists each of them, and each suite with its command

  @id:carries-cited-decisions
  Scenario: The decisions the specs rest on
    Given specs of the component that cite "keep-ledger", and another accepted decision that no spec of the component cites
    When its packet is built
    Then the section "Decisions" holds the title and the "Decision" text of "keep-ledger" and not the other one

  @id:marks-reference-as-context
  Scenario: Reference sections come last and are labelled
    Given a reference section that explains a spec of the component
    When its packet is built
    Then the last section is "Reference (context, not a requirement)" and holds the text of that section

  @id:packet-is-deterministic
  Scenario: The same project always gives the same packet
    Given any project
    When the packet of a component is built twice, with the files listed in different orders
    Then both packets are byte for byte the same

  @id:unknown-component
  Scenario: The packet of a component that does not exist
    Given a project with no component "accounts"
    When its packet is asked for
    Then the engine answers "unknown_component"
