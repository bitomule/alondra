@spec:antigravity.conformance
@decision:antigravity-reaches-google-through-agy
Feature: Antigravity holds to the providers' conformance suite
  Every provider of Alondra is held to one conformance suite: a set of
  transcripts, one per behaviour, played through the provider's own reading
  of its host's messages inside a real store, and compared with the neutral
  facts every provider must produce. The Antigravity transcripts are written
  from the Agent Client Protocol, not recorded from a real agy. The binding
  declares what ACP offers: offering input, interrupting, force stopping,
  durable resume, setting the model, setting the permission mode and
  decisions; no background work, compaction or MCP controls, so the
  scenarios that need those are not played. A difference from the neutral
  facts is allowed only when it is listed as a known divergence with its
  cause, and the suite fails once that divergence no longer happens, so it
  is removed with the fix.

  @id:simple-turn
  Scenario: One input, one turn
    Given the transcript "simple-turn"
    When it is played
    Then the facts are "Available", "InputHeld(input-1)", "TurnStarted(T1, input-1)", "ModelAnswered", "TurnEnded(T1, Completed)" and "Reconciled(idle, live=[])"

  @id:several-turns
  Scenario: Two inputs one after the other
    Given the transcript "several-turns"
    When it is played
    Then each input is held, starts its own turn, is answered and ends completed, and the node is idle after each

  @id:input-during-turn-waits-for-the-prompt
  Scenario: An input sent during a turn is held only once the prompt before it is answered
    Given the transcript "input-during-turn"
    When it is played
    Then "InputHeld(input-2)" comes after "TurnEnded(T1, Completed)" and "Reconciled(idle, live=[])", because the host sends the second prompt only once agy answered the first
    And this is the known divergence of Antigravity for "input-during-turn"

  @id:interrupt
  Scenario: An interrupted turn
    Given the transcript "interrupt"
    When it is played
    Then the turn ends "Failed(interrupted)" and the node is idle

  @id:process-dies-then-resume
  Scenario: agy dies mid-turn and the next host resumes the session
    Given the transcript "process-dies-then-resume"
    When it is played
    Then the facts are "Available", "InputHeld(input-1)", "TurnStarted(T1, input-1)", "HostEnded", "InputHeld(input-2)", "TurnStarted(T2, input-2)", "ModelAnswered", "TurnEnded(T2, Completed)" and "Reconciled(idle, live=[])"

  @id:finished-input-not-taken-again
  Scenario: Something agy says after a turn ended opens a turn of its own
    Given the transcript "finished-input-not-taken-again"
    When it is played
    Then after the input's turn ends, the facts are "TurnUnprompted(T2)", "ModelAnswered", "TurnEnded(T2, Completed)" and "Reconciled(idle, live=[])", and the finished input is not taken again

  @id:consumed-input-named-by-provider
  Scenario: An input agy refused before any update is dropped
    Given the transcript "consumed-input-named-by-provider"
    When it is played
    Then the facts are "Available", "InputHeld(input-1)", "InputDropped(input-1)" and "Reconciled(idle, live=[])"

  @id:decisions
  Scenario: A permission and a question during a turn
    Given the transcript "decisions"
    When it is played
    Then the facts include "Asked(D1, E1, Permission)", "Settled(E1, answered, continued)", "Asked(D2, E2, Question)" and "Settled(E2, unanswered, continued)" before "TurnEnded(T1, Completed)"
