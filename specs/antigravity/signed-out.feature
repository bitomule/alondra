@spec:antigravity.signed-out
@decision:antigravity-refused-key-signs-the-account-out
Feature: A signed-out Antigravity account holds its task until the user signs in
  If agy refuses a node's account before it takes the node's input,
  because the account is not signed in or has no usable key, the host
  reports it and Alondra marks the account signed out, so no other work
  starts on it. The task waits on a card that says how to sign in and lets
  the user try again once they have. Alondra never signs in for the user and
  never starts another host for the task while it waits.

  @id:refusal-signs-the-account-out
  Scenario: A host refused for its sign-in signs its account out and asks to sign in
    Given a signed-in default Antigravity account running a task's turn
    When the task's host is lost because its account was refused for its sign-in
    Then the account is "SignedOut"
    And the task needs the user, on a card titled "Sign in to Antigravity outside Alondra"
    And the card offers to try the task again once the user has signed in

  @id:no-other-host-while-signed-out
  Scenario: A task whose account is not signed in waits on the card and starts no other host
    Given a task on an Antigravity account whose host refuses its first input with "authentication_required" before agy takes it
    When the daemon rests for ten minutes
    Then the home screen shows "Sign in to Antigravity outside Alondra"
    And exactly one host started, and it played its transcript to the end
