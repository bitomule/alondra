@spec:stage-handover.boundary
@decision:a-stage-handover-references-the-plan-and-the-specs
Feature: What the stage handover may reach
  The packet a new stage starts from and the budget it must fit are worked out from the
  domain's state and the paths their caller gives them. Writing the files the packet names is
  the caller's job, so the same state always gives the same packet, in a test and in the daemon.

  @id:reaches-nothing-outside @boundary:dependencies
  Scenario: The packet and its budget never reach the disk, the network, a program or a thread
    Given the files the component "stage-handover" owns
    When they are searched for "std::fs", "std::process", "std::net" and "std::thread"
    Then none of them is used

  @id:depends-on-no-component @boundary:dependencies
  Scenario: The stage handover depends on no other component
    Given the boundary of "stage-handover"
    When the components it depends on are listed
    Then the list is empty
