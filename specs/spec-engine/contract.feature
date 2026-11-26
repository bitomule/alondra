@spec:spec-engine.contract
@decision:engine-reads-through-its-caller
@decision:component-boundaries
Feature: What the engine promises to the code around it
  The engine is a library other crates call. These are the promises of its
  boundary that a new implementation must keep even though no caller would
  notice a break at once.

  @id:public-surface-matches-snapshot @boundary:surface
  Scenario: The public surface is the committed one
    Given the Rust sources under "crates/spec-engine/src"
    When their public items are extracted with "tools/public_surface.py"
    Then the result equals "specs/spec-engine/surface.txt" line for line

  @id:uses-no-other-crate-of-the-workspace @boundary:dependencies
  Scenario: The engine stands alone
    Given "crates/spec-engine/Cargo.toml"
    When its dependencies are listed
    Then they are exactly "quick-xml", "serde", "serde_json", "sha2" and "toml", and none is a crate of this workspace

  @id:reads-only-through-its-caller @boundary:dependencies
  Scenario: The engine never reaches the disk, the network, the clock or another program
    Given the Rust sources under "crates/spec-engine/src"
    When they are searched for "std::fs", "std::process", "std::net", "std::env", "std::time" and "std::thread"
    Then none of them is used
