@spec:antigravity.sessions
@decision:antigravity-handoff-when-resume-fails
Feature: A node's agy session lasts across hosts and turns
  The host keeps, for each node, which agy session it opened, in a private
  file beside the host's state folder. A later host of the same node resumes
  that session instead of starting a new one, so the conversation goes on
  where it was. The host sends one prompt at a time, relays what agy says
  during a turn in order, and ends a turn by an interrupt, a forced stop or
  agy's own death in a way the owner can always account for.

  @id:prompt-turn-sequence
  Scenario: A turn is reported in order, from the prompt sent to agy's answer
    Given a node on the default Google account
    When it offers the input "Fix the parser"
    Then agy receives "session/prompt" with the session's id and that text, under an id "alondra:<n>"
    And the host reports, in this order, "alondra/sessionReady", "alondra/promptSent" with the input and that turn id, each "session/update" agy sent with that turn id, and "alondra/promptEnded" with agy's stop reason "end_turn"

  @id:session-stored-privately
  Scenario: The session a node opened is kept in a private file
    Given a node on the default Google account whose first turn ended
    When its stored session is read
    Then the folder "native-sessions" beside the host's state folder has mode 0700
    And the file named after the node's conversation has mode 0600 and holds "acpSessionId", "account" "default", "access" "google" and "agyVersion" "1.5.14"

  @id:later-host-resumes
  Scenario: A later host of the same node resumes the stored session silently
    Given a node whose first host opened agy session "fixture-acp-session-1", ran a turn and closed
    When a later host of the node opens with "resume" and offers "Go on"
    Then agy receives "session/resume" for "fixture-acp-session-1" with the node's working folder, Alondra's MCP server and no "_meta"
    And it receives no second "session/new" and never "session/load"
    And the prompt it receives is only "Go on"
    And "alondra/sessionReady" says "resumed" true with the later host's process generation

  @id:not-resumed-elsewhere
  Scenario Outline: A stored session is not resumed for another account, another access or a session agy lost
    Given a node whose first host ran a turn and was given a handoff of the conversation so far
    When a later host opens it with <change> and offers "Go on"
    Then a new session is opened and "alondra/sessionReady" says "resumed" false
    And the first prompt of the new session is the instructions, the handoff and "Go on", each separated by a blank line
    And the stored session now names the later host's account and access

    Examples:
      | change                                                     |
      | the named account "work"                                   |
      | the access "enterprise"                                    |
      | an agy that answers "session/resume" that it has no such session |

  @id:kept-once-instructions-sent
  Scenario: A new session is kept only once the prompt carrying its instructions is sent
    Given an agy that does not advertise appending to its system prompt, and a node whose host opened a session and closed before any prompt
    When its stored session is looked for
    Then there is none
    And a later host opens a new session whose first prompt carries the instructions, the handoff and the input, and keeps that session

  @id:signed-out-resume-refused
  Scenario: A stored session of an account that is now signed out is refused, not replaced
    Given a node whose first host stored its session
    When a later host resumes it and agy answers "session/resume" that the account is not signed in
    Then the input is refused as never offered with "authentication_required" and the host reports "alondra/accountBlocked"
    And agy received one "session/resume" and no second "session/new"

  @id:stored-file-must-be-private
  Scenario: A stored session file that is not private is refused, never resumed or replaced
    Given a node whose stored session file was changed to mode 0644
    When a later host opens with "resume"
    Then the input is refused as never offered with "native_initialization_failed"
    And the file keeps mode 0644 and no new session is opened

  @id:one-prompt-at-a-time
  Scenario: An input offered during a turn is sent once agy answered the prompt before it
    Given a node whose first turn waits on a permission request
    When a second input is offered during that turn
    Then agy receives the second "session/prompt" only after it answered the first
    And the host reports "alondra/promptSent" for the second input after "alondra/promptEnded" of the first

  @id:prompt-error-keeps-no-text
  Scenario: A prompt agy answers with an error is reported by its code, never its message
    Given an agy that answers a prompt with the error code -32603, a message and a data object
    And then answers the next prompt with the error code -32603, a message and data that is not an object
    When the node offers both inputs and a third
    Then the host reports "alondra/promptFailed" for the first with the code and the data object, and for the second with the code alone
    And no observation holds either message
    And the third input runs and ends with "alondra/promptEnded"

  @id:interrupt-cancels-the-turn
  Scenario: Interrupting cancels the turn and agy ends it cancelled
    Given a turn in progress
    When the node interrupts it
    Then agy receives the notification "session/cancel" with the session's id
    And agy's answer to the prompt ends it with the stop reason "cancelled"
    And the session has no failure

  @id:force-stop-kills-agy
  Scenario: Force stop ends agy even when it ignores a cancel and SIGTERM
    Given an agy that ignores "session/cancel" and SIGTERM in the middle of a turn
    When the turn is interrupted and the node is force stopped
    Then agy received "session/cancel" and "session/close"
    And agy ends by SIGKILL
    And the host reports no "alondra/promptEnded" and no "alondra/transportFailed": a stop the owner asked for is not a transport failure
    And the host's temporary folder is removed

  @id:agy-dies-mid-turn
  Scenario: agy dying in the middle of a turn is reported after its last evidence
    Given a turn in which agy sent "Last words" and then a permission request that is still open
    When agy exits with the code 3
    Then the host reports, in this order, the update with "Last words", the request, the request settled "cancelled", "alondra/transportFailed" with "native_process_exited" and agy's exit with the code 3
    And it reports no "alondra/promptEnded" for that turn
    And a later input is refused with "native_process_exited"

  @id:close-is-orderly
  Scenario: Closing a session tells agy before it ends
    Given an open session whose agy advertises closing a session
    When the session closes
    Then agy receives "session/close" before it ends
    And the session has no failure, the host reports no "alondra/transportFailed", and it reports one exit of agy
