@spec:disk-gate.boundary
@decision:component-boundaries
Feature: What the disk gate may reach
  The disk gate decides from the domain's state, the samples of the Mac and the estimates it
  keeps. Its estimates are measured from the disk's own samples, never from what a program prints,
  so the same samples give the same reservations in a test and in the daemon.

  @id:depends-on-no-component @boundary:dependencies
  Scenario: The disk gate depends on no other component
    Given the boundary of "disk-gate"
    When the components it depends on are listed
    Then the list is empty
