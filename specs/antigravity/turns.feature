@spec:antigravity.turns
Feature: What a node's agy says becomes the task's turns
  The Antigravity host forwards what agy says over the Agent Client
  Protocol ("session/update" notifications) and adds notices of its own
  ("alondra/sessionReady", "alondra/promptSent", "alondra/promptEnded",
  "alondra/promptFailed", "alondra/inputRefused", "alondra/accountBlocked",
  "alondra/transportFailed"). Alondra reads them as the turns of the task: when
  agy took an input, when its model answered, how the turn ended, and what
  the task learns from the tools the model ran. It decides on the fields of
  these messages, never on the text the model or agy wrote.

  @id:updates-of-another-session-ignored
  Scenario: Updates of another session are not the task's
    Given a node whose agy session "session-1" is ready
    When agy sends an "agent_message_chunk" whose "sessionId" is "session-2", null, a number, or missing
    Then none of them is read as the task's
    And before any session is ready, no update is read as the task's

  @id:session-updates-open-no-turn
  Scenario: Updates about the session open no turn, and the model's first word does
    Given a node whose input was sent to agy as a prompt
    When agy sends a "usage_update", then an "agent_thought_chunk", a "plan", a pending "tool_call", two "agent_message_chunk" with "Done" and " now." and a "session_info_update"
    Then the "usage_update" opens no turn and the "agent_thought_chunk" starts the input's turn
    And the model is said to have answered once, at the first "agent_message_chunk", and the turn's answer is "Done now."
    And "end_turn" ends the turn completed and the node idle, and the next prompt's first "agent_message_chunk" starts a new turn that answers again

  @id:stop-reasons
  Scenario Outline: How agy ends a prompt decides how its turn ends
    Given a node whose input was sent to agy as a prompt and nothing else was said
    When agy ends the prompt with the stop reason "<reason>"
    Then the input's turn starts and ends <outcome>, and no prompt is open

    Examples:
      | reason            | outcome                                 |
      | end_turn          | completed                               |
      | cancelled         | failed with the code "interrupted"      |
      | max_tokens        | failed with the code "max_tokens"       |
      | max_turn_requests | failed with the code "max_turn_requests" |
      | refusal           | failed with the code "refusal"          |

  @id:error-before-any-update-drops-the-input
  Scenario: A prompt agy refuses before saying anything never took its input
    Given a node whose input was sent to agy as a prompt
    When agy answers the prompt with the JSON-RPC error -32603 or -32602 and no data, before any update
    Then the input is dropped, not consumed, the node is idle and no prompt is open

  @id:errors-by-their-structured-data
  Scenario Outline: A prompt that fails is told by its error's code and structured data, never its text
    Given a node whose prompt agy already started answering
    When agy answers the prompt with the error <error>
    Then the turn ends <outcome>

    Examples:
      | error                                                                    | outcome                                       |
      | code -32603 and data {"code": 429, "status": "RESOURCE_EXHAUSTED"}       | as an API failure "rate_limit" with status 429 |
      | code -32603 and data {"status": "RESOURCE_EXHAUSTED"}                    | as an API failure "rate_limit" with status 429 |
      | code -32603 and data {"code": 401, "status": "UNAUTHENTICATED"}          | as an API failure "authentication_failed" with status 401 |
      | code -32603 and data {"code": 403, "status": "PERMISSION_DENIED"}        | as an API failure "permission_error" with status 403 |
      | code -32603 and data {"code": 503, "status": "UNAVAILABLE"}              | as an API failure "api_error" with status 503 |
      | code -32603 and no data                                                  | failed with the code "agy_-32603"              |
      | code -32000 and the data "Quota exceeded, retry later"                  | failed with the code "agy_-32000"              |

  @id:api-failure-before-any-update-is-a-failed-turn
  Scenario Outline: A model API failure before the model said anything still ends a turn
    Given a node whose prompt agy has only reported a "usage_update" for
    When agy answers the prompt with code -32603 and data <data>
    Then the input's turn starts and ends as an API failure "<error>", and no prompt is open

    Examples:
      | data                                       | error                 |
      | {"code": 429, "status": "RESOURCE_EXHAUSTED"} | rate_limit         |
      | {"code": 401}                              | authentication_failed |
      | {"code": 503}                              | api_error             |

  @id:prompt-of-a-dead-process
  Scenario: The answer to a prompt the last agy held decides nothing
    Given a node whose prompt was sent to an agy that then died, so the task forgot that turn
    When an answer to that prompt arrives
    Then it changes nothing
    And an answer that names another prompt while one is open is refused as an invalid observation

  @id:turn-agy-opens-by-itself
  Scenario: A turn agy opens by itself ends when its model call reports usage
    Given a node with a ready session and no prompt of Alondra's open
    When agy sends a "user_message_chunk" and an "agent_message_chunk", then a "usage_update" with "used" 900
    Then the first opens a turn no input asked for, the second is its model's answer, and the "usage_update" ends that turn and leaves the node idle
    And a turn agy opens by itself afterwards ends when Alondra sends its next prompt, before that prompt's input is held

  @id:pull-requests-only-from-a-successful-close
  Scenario: The task re-reads its pull requests only after a "gh pr close" that succeeded
    Given a node whose model runs a command through agy with kind "execute"
    When the command "gh pr close 6" finishes with exit code 0
    Then the task is told its pull requests changed
    And "gh pr close 6" with exit code 1, "gh pr create --fill", "make test && gh pr create --fill" and "gh pr view 6" change nothing, even when their output names a pull request
    And a finished command's output becomes context of the task

  @id:refusal-holds-only-before-agy-took-the-input
  Scenario: Only a refusal that lasts holds the task, and only before agy took the input
    Given the host reports the codes "authentication_required", "native_version_unsupported", "native_version_unreadable" and "native_incompatible" as an "alondra/accountBlocked" or an "alondra/inputRefused" with "nativeSubmitted" false
    When the task reads them
    Then each holds the task with that code's message
    And the same codes with "nativeSubmitted" true, the codes "native_spawn_failed", "native_initialization_timeout", "native_process_exited" and "agy_-32603", and an "alondra/transportFailed" hold nothing: the task tries again with its next input

  @id:only-a-refused-sign-in-asks-to-sign-in
  Scenario: Only a refused sign-in or key asks the user to sign in
    Given the host refuses an input before agy took it
    When the code is "authentication_required" in an "alondra/accountBlocked" or an "alondra/inputRefused" with "nativeSubmitted" false
    Then the task asks the user to sign in
    And "native_version_unsupported", or "authentication_required" with "nativeSubmitted" true, does not ask to sign in

  @id:a-session-that-never-opened-names-its-step
  Scenario: A session that never opened says the step agy did not answer
    Given the host reports "alondra/transportFailed" with code "native_initialization_timeout" and step "session_new"
    When the failure is read
    Then it reads "native_initialization_timeout at session_new"
    And a failure with no step reads as its code alone, and an "alondra/accountBlocked" is not a session failure

  @id:every-code-has-its-message
  Scenario: Every failure code the host or a turn reports has a message for the user
    Given the codes "authentication_required", "native_version_unsupported", "native_version_unreadable", "native_incompatible", "native_spawn_failed", "native_initialization_timeout", "native_initialization_failed", "alondra_mcp_missing", "native_process_exited", "native_input_closed", "native_output_closed", "native_protocol_failed", "native_response_uncertain", "native_prompt_uncertain", "native_session_not_durable", "native_decision_not_durable", "native_decision_capacity", "native_request_identity_conflict", "max_tokens", "max_turn_requests", "refusal", "interrupted" and "agy_-32603"
    When their messages are read
    Then each has a message that is not empty, and an unknown code such as "something_new" has none

  @id:reconciled-work-is-antigravitys
  Scenario: Work the task reconciles is noticed as Antigravity's
    Given the work "antigravity:1:process-1:work:call_1"
    When its continuation is named
    Then it is "antigravity-reconciled:antigravity:1:process-1:work:call_1"
    And "antigravityish:call_1" is not read as Antigravity's

  @id:host-lane-refuses-what-the-host-did-not-announce
  Scenario: An action the host did not announce is refused before it reaches the host
    Given an attached Antigravity host that did not announce the MCP controls
    When the task asks it to clear an MCP server's sign-in
    Then the action is refused before any offer and the host receives nothing after the attach

  @id:task-host-needs-decisions-and-no-orchestrator-host
  Scenario: A task host must carry decisions, and no orchestrator host is attached
    Given an Antigravity host that announces "streaming_input", "owner_attach" and "commit_ack"
    When a task's host is attached to it
    Then it is refused for a missing capability, and accepted once it also announces "exact_decisions"
    And an orchestrator execution requires no capability set, because Antigravity runs no orchestrator

  @id:host-that-ends-before-its-receipt
  Scenario: An input waits for its receipt as long as the host lives
    Given an attached Antigravity host that reads an input and then ends without a receipt
    When the task waits for that input's receipt
    Then the wait ends at once with an error, and nothing waited on a clock to learn it

  @id:host-output-before-close-is-read
  Scenario: What a host wrote before it closed is still read
    Given a host that wrote "result" and then closed its end of the socket
    When the daemon reads the socket
    Then it reads the 6 bytes of "result", then the end of the host

  @id:events-queued-behind-a-receipt
  Scenario: Events that arrive while a command waits for its receipt are kept up to the host's own limit
    Given a command that waits for its receipt
    When two tool results of 900,000 bytes and 1,000 small events arrive meanwhile
    Then all of them are queued
    And an event that arrives when 16,384 events are already queued, or that takes the queue past 64 MiB, is refused

  @id:event-cursors-are-contiguous
  Scenario: The host's events are read in order, without a gap or a repeat
    Given a lane whose committed cursor is 12
    When events arrive with cursors 11, 12, 14, 13 and 13 again
    Then 11, 12 and 13 are read in that order and 14 and the repeated 13 are refused
    And on a lane with nothing committed, cursors 0 and 2 are refused and 1 is read

  @id:deadlines-are-absolute
  Scenario: A frame read or a command written piece by piece cannot outlast its deadline
    Given a deadline one second away
    When a frame arrives one byte at a time or a command is written one byte at a time, past the deadline
    Then each fails with a timeout, and the frame reader refuses any later read
    But a socket timeout in real time keeps waiting while the daemon's clock is before the deadline
