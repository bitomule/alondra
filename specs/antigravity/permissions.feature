@spec:antigravity.permissions
Feature: agy's permission requests become the node's decisions
  A permission agy asks for a tool call during a turn
  ("session/request_permission") becomes a decision of the
  node: a permission when agy offers an option to allow once or to
  reject once, and otherwise a question whose options are agy's own. Alondra
  answers with exactly one of agy's options, never one that would write a
  lasting rule into the user's agy settings ("allow_always",
  "reject_always"). The host answers by itself only what needs no person:
  Alondra's own tools in a mode that runs without asking, and everything else
  in the mode that never asks.

  @id:request-becomes-a-permission
  Scenario: A request with an allow once option is asked as a permission
    Given a turn in which agy asks permission for the tool call "call_1" titled "make test", offering "Allow" (allow_once), "Always allow" (allow_always) and "Deny" (reject_once)
    When the host reports it
    Then it reports a decision of kind "permission" with the request id "string:1" and the tool use id "call_1"
    And the decision names the provider "antigravity", the method "session/request_permission", the tool "make test", agy's options as sent and agy's session

  @id:allow-selects-allow-once
  Scenario: Allowing selects the allow once option and nothing else
    Given a permission request offering "Always allow" (allow_always), "Allow" (allow_once) and "Deny" (reject_once)
    When the node allows it
    Then agy receives the option "allow" selected
    And the request is settled "answered"

  @id:never-an-always-option
  Scenario: Alondra never selects an option that writes a lasting rule
    Given a permission request offering "Always allow" (allow_always) and "Deny" (reject_once) and no allow once option
    When the node allows it, or answers it with the "allow_always" option
    Then the answer is refused and agy receives nothing

  @id:stale-or-repeated-answer-refused
  Scenario: An answer for another epoch, or a second answer, is refused
    Given an open permission request
    When the node answers it with an epoch that is not the request's
    Then that answer is refused
    And after one answer is taken, answering the same request again is refused

  @id:deny-selects-reject-once
  Scenario: Denying selects the reject once option
    Given a permission request offering "Allow" (allow_once) and "Deny" (reject_once)
    When the node denies it with the message "Not now"
    Then the host is told to deny with "Not now", and agy receives the option "deny" selected

  @id:request-without-allow-or-reject-is-a-question
  Scenario: A request that offers neither allow once nor reject once is a question
    Given a permission request titled "make test" offering only "Use the new parser" and "Keep the old parser", both of kind "other"
    When the host reports it
    Then it reports a decision of kind "question" asking "make test" with the options "Use the new parser" and "Keep the old parser"
    And answering with the option "option-b" selects "option-b" for agy, while an option agy did not offer is refused
    And denying the question answers agy "cancelled"

  @id:question-answered-by-its-own-options
  Scenario: A question is answered with exactly one of its own options
    Given a question offering "Keep the current schema", "Migrate the schema" and "Remember: always migrate" (allow_always)
    When the node answers it
    Then the answer "Migrate the schema" selects that option
    And the answers "Remember: always migrate", "Something the request does not offer" and two answers at once are refused

  @id:other-answers-are-refused
  Scenario: Answers that are not Antigravity's to take are refused
    Given a permission request offering "Allow" (allow_once)
    When the node answers it as superseded, declined, accepted with text, or with a free answer
    Then each answer is refused

  @id:own-tools-allowed-in-auto-mode
  Scenario: In agy's auto mode, Alondra's own tools run without a card
    Given an agy that offers the modes "default", "auto", "accept-edits" and "plan", and a node in Alondra's default mode
    When agy asks permission for the tool call named "mcp_alondra_present_result"
    Then the host has set agy's mode to "auto" and answers the request itself with the "allow" option, and no decision is reported
    And a request for "mcp_alondraish_tool" is reported as a decision
    And a request whose title starts with "mcp_alondra_present_result" but names no tool is reported as a decision

  @id:own-tools-asked-in-plan-mode
  Scenario: In plan mode, Alondra's own tools are asked like any other
    Given an agy that offers an auto mode and a node that switches to the mode "plan"
    When agy asks permission for the tool call named "mcp_alondra_present_result"
    Then agy received "session/set_mode" "plan" and the request is reported as a decision

  @id:dont-ask-rejects-the-rest
  Scenario: dontAsk runs in agy's default mode and rejects every request but Alondra's own tools
    Given an agy that offers an auto mode and a node in the mode "dontAsk"
    When agy asks permission for "mcp_alondra_present_result" and then for "rm -rf build"
    Then agy's mode is set to "default"
    And the first request is answered "allow" and the second "deny", both without a decision

  @id:own-answers-are-not-counted
  Scenario: Requests the host answers by itself never use up the decisions a session keeps
    Given an agy in auto mode that asks 300 times for "mcp_alondra_present_result" and then asks for a command
    When the turn runs
    Then the host answers the 300 requests itself
    And the request "string:301" is reported as a decision, can be allowed, and the turn ends "end_turn"

  @id:interrupt-cancels-pending-request
  Scenario: Interrupting a turn cancels its open request
    Given a turn waiting on a permission request
    When the node interrupts the turn
    Then the request is settled "cancelled" and agy's request is answered "cancelled"
    And a later answer to that request is refused
