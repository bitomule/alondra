@spec:task-flow.boundary
@decision:component-boundaries
Feature: What the task flow rules may reach
  The rules of this component decide on the domain's state alone. They read a worktree
  only through what the spec engine and the domain's ports hand them, so the same
  commands give the same state in a test, in the daemon and on a replay.

  @id:rules-reach-nothing-outside @boundary:dependencies
  Scenario: The rules never reach the disk, the network, a program or a thread
    Given the files the component "task-flow" owns
    When they are searched for "std::fs", "std::process", "std::net" and "std::thread"
    Then none of them is used

  @id:depends-only-on-spec-engine @boundary:dependencies
  Scenario: The only component the rules depend on is the spec engine
    Given the boundary of "task-flow"
    When the components it depends on are listed
    Then they are exactly "spec-engine"
