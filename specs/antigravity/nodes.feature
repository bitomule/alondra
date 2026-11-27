@spec:antigravity.nodes
@decision:antigravity-blocked-tools-hold-the-account
Feature: Nodes run on Antigravity accounts
  A task node can run on an Antigravity account. Alondra starts the
  Antigravity host for it, the host starts the account's agy, and the node
  talks to agy over ACP: its instructions, the task's input, Alondra's own
  tools as an MCP server called "alondra", and the task's effort as the
  session's thinking level. The account's credentials reach only that agy.
  A node whose agy or host ends goes on in the same agy session with a new
  host, and a daemon that restarts takes over the host that still runs
  instead of starting a second agy. These scenarios run the release's
  Antigravity host on the fake agy of the host's own tests.

  @id:turn-through-host-and-agy
  Scenario: A node on the default Google account runs its turn through the host and agy
    Given the default Antigravity account with access "google" and a task whose input is "Write the release notes through Antigravity" with effort "high"
    When the task's node is launched and its turn settles
    Then the task's provider is Antigravity and its node's driver is "antigravity-acp"
    And agy was started as "agy acp" with "AGY_CLI_DISABLE_AUTO_UPDATE" set to "1" and no "GEMINI_HOME"
    And agy was never asked to "authenticate"
    And exactly one "session/new" was sent, naming the MCP server "alondra" and carrying the node's instructions in "_meta.antigravity.systemPrompt.append"
    And the first "session/prompt" holds the text "Write the release notes through Antigravity"
    And a "session/set_config_option" set "thought_level" to "high"
    And what agy answered, "Hello from Antigravity", is the node's answer in the task's context

  @id:key-reaches-only-its-agy
  Scenario: An API key account's key reaches only its own agy, through its environment
    Given a named Antigravity account "studio" with access "api_key" and a key in its key file
    When the account's node is launched and stopped
    Then agy was asked to "authenticate" with "gemini-api-key"
    And agy was started as "agy acp --gemini-dir <folder>" with "GEMINI_HOME" set to the account's folder
    And the "GEMINI_API_KEY" agy got is the key of the account's file, and agy got no "GOOGLE_API_KEY"
    And no file under Alondra's home other than the key file holds the key

  @id:interrupt-cancels-the-prompt
  Scenario: Interrupting a node cancels its prompt and ends its turn
    Given a node on the default Antigravity account whose agy is working on a prompt
    When the user interrupts the node's turn
    Then agy received exactly one "session/cancel"
    And the task is no longer working

  @id:force-stop-ends-host-and-agy
  Scenario: Stopping a node ends its host and its agy
    Given a node on the default Antigravity account whose agy is working on a prompt
    When the node is stopped
    Then the host and its agy ran in one process group of at least two processes
    And no process of that group is alive afterwards

  @id:restarted-daemon-adopts-the-host
  Scenario: A restarted daemon takes over the running host
    Given a node on the default Antigravity account whose agy is working on a prompt
    When the daemon restarts and takes the node over
    Then it drives the same host process group
    And agy received "initialize" once and "session/prompt" once: no second agy started and no input was sent again

  @id:dead-agy-resumes-the-session
  Scenario: A node whose agy died goes on in the same session with a new host
    Given a node on the default Antigravity account whose agy opened the session "fixture-acp-session-1" and is working on a prompt
    When its agy is killed and the node's ended host is replaced
    Then the new host sends "session/resume" for "fixture-acp-session-1"
    And exactly one "session/new" was ever sent
    And the node starts its next turn

  @id:removed-key-starts-no-later-agy
  Scenario: After a key is removed, no later agy of the account starts
    Given a node on the named "api_key" account "studio" whose agy is working on a prompt
    When the account's key is removed, its agy ends and the node's ended host is replaced
    Then agy was started only once, before the key was removed
    And the account's key file does not exist

  @id:task-moves-to-antigravity
  Scenario: A task moves to an Antigravity account whose catalog offers a model
    Given a task on a Codex account and a signed-in default Antigravity account whose model catalog offers a model
    When the task is moved to the Antigravity account
    Then the move is offered and the task is moving to the Antigravity account

  @id:key-change-recorded-against-the-current-state
  Scenario: A key change is recorded whatever committed since it was read
    Given a named "api_key" account "studio" without a key
    When the user's key change for "studio" arrives after another change of the domain committed
    Then the change is recorded: "studio" has a key and its credential generation is 1

  @id:no-orchestrator-batches
  Scenario: Antigravity is never asked for the orchestrator's batches
    Given the registry of the providers that run the orchestrator's batches, with a Codex provider added
    When the missing batch providers are listed
    Then Claude is listed and Antigravity is not

  @id:turn-cut-and-resumed
  Scenario: A turn cut by its host dying goes on in the same session with a new host
    Given a node on Antigravity whose host dies with exit 137 while its first turn runs
    When the daemon replaces the host
    Then the new host is offered "Check where you were" and reports the same session, resumed, in process generation "2"
    And after that turn ends without a result, the node is offered "Your turn ended without a result"
    And exactly two hosts started, and each played its transcript to the end
