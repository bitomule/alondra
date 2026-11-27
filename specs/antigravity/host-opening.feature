@spec:antigravity.host-opening
Feature: The Antigravity host opens agy for one node
  Each node on an Antigravity account runs in an Antigravity host: a process
  of Alondra's that starts "agy acp" as its only child and speaks the Agent
  Client Protocol with it over standard input and output. Before any input is
  offered, the host checks that the agy it started is a version and an agent
  Alondra can drive, builds agy's environment from the account alone, gives
  agy Alondra's own MCP server and the node's instructions, and applies the
  node's model and thinking level. Anything that keeps the session from
  opening refuses the input as never offered, with a code that says why.

  @id:announces-itself
  Scenario: The host declares its version and what it can do
    Given the Antigravity host
    When its declared version and capabilities are read
    Then its SDK version is "antigravity-acp-1"
    And its capabilities are "streaming_input", "exact_decisions", "owner_attach", "commit_ack", "interrupt", "force_stop", "set_model" and "set_permission_mode"

  @id:starts-agy-acp-and-initializes
  Scenario: The host starts "agy acp" and introduces itself as Alondra
    Given a node on the default Google account
    When the host opens its session
    Then the only process it starts is agy with the arguments "acp"
    And agy receives "initialize" with protocol version 1, no file system or terminal capability, and client "alondra" titled "Alondra" with the host's release as its version
    And the host sends no "authenticate": agy signs a Google account in by itself

  @id:session-ready-says-what-opened
  Scenario: The host reports the session it opened
    Given a node on the default Google account whose agy reports version "1.5.14" and model "gemini-4-carbon"
    When the session opens
    Then the host reports "alondra/sessionReady" with agy's session id, the node's conversation id, the working folder, the model "gemini-4-carbon", the process generation, "resumed" false and "agyVersion" "1.5.14"

  @id:versions-read-from-initialize
  Scenario Outline: agy's version is read from its answer to initialize
    Given an agy that answers "initialize" with version "<version>"
    When a node offers its first input
    Then <outcome>

    Examples:
      | version | outcome                                                                                   |
      | 1.5.11  | the input is refused as never offered with "native_version_unsupported"                   |
      | unknown | the input is refused as never offered with "native_version_unreadable"                    |
      |         | the input is refused as never offered with "native_version_unreadable"                    |

  @id:refused-version-sends-nothing-else
  Scenario: A version the host refuses ends the session before anything else reaches agy
    Given an agy that reports version "1.5.11"
    When a node offers an input
    Then the host reports "alondra/inputRefused" with no session, that input, "nativeSubmitted" false and the code "native_version_unsupported"
    And it reports "alondra/transportFailed" with that code and then that agy exited
    And "initialize" is the only message agy received and agy was started once
    And a later input is refused with the same code

  @id:minimum-version
  Scenario Outline: agy 1.5.12 and later are supported
    Given an agy version "<version>"
    When the host compares it with the minimum
    Then it is <result>

    Examples:
      | version | result      |
      | 1.5.12  | supported   |
      | 1.10.0  | supported   |
      | 2.0.0   | supported   |
      | 1.5.11  | unsupported |
      | 0.9.99  | unsupported |

  @id:another-agent-is-incompatible
  Scenario Outline: An agent Alondra cannot drive is refused as incompatible
    Given an agy that answers "initialize" with <difference>
    When a node offers its first input
    Then the input is refused as never offered with "native_incompatible"
    And "initialize" is the only message agy received

    Examples:
      | difference                        |
      | the agent name "Other Agent"      |
      | protocol version 2                |
      | no session resume capability      |

  @id:environment-of-each-access
  Scenario Outline: Each access gives agy its own credentials and nothing it inherited
    Given a host whose own environment holds Google credentials of other origins for every credential variable
    And an account "<account>" with access "<access>" and <settings>
    When the host builds agy's environment
    Then the credential variables agy gets are exactly <credentials>

    Examples:
      | account | access        | settings                                                   | credentials                                                                                                                  |
      | default | google        | nothing else                                               | none                                                                                                                         |
      | work    | google        | its folder                                                 | "GEMINI_HOME" set to its folder, "AGY_ACP_FORCE_FILE_STORAGE" "1" and "SSH_CONNECTION" "127.0.0.1 0 127.0.0.1 0"               |
      | work    | enterprise    | its folder, a project and a location                       | those three, with "GOOGLE_CLOUD_QUOTA_PROJECT" and "GOOGLE_CLOUD_LOCATION" set to the project and the location                 |
      | default | enterprise    | nothing else                                               | none                                                                                                                         |
      | work    | api_key       | its folder and its key file                                | "GEMINI_HOME" set to its folder and "GEMINI_API_KEY" set to the key                                                          |
      | work    | cloud_project | its folder, its key file, a project and a location         | "GEMINI_HOME", "GOOGLE_API_KEY" set to the key, "GOOGLE_CLOUD_QUOTA_PROJECT" and "GOOGLE_CLOUD_LOCATION"                      |
      | work    | cloud_project | its folder, a credentials file, a project and a location   | "GEMINI_HOME", "AGY_ADC_AUTH" "true", "GOOGLE_APPLICATION_CREDENTIALS" set to the file, "GOOGLE_CLOUD_QUOTA_PROJECT" and "GOOGLE_CLOUD_LOCATION" |
      | default | cloud_project | a project                                                  | "AGY_ADC_AUTH" "true" and "GOOGLE_CLOUD_QUOTA_PROJECT" set to the project                                                     |

  @id:environment-always-set
  Scenario: agy never updates itself and works in the host's private folders
    Given a node on the default Google account
    When the host builds agy's environment
    Then "AGY_CLI_DISABLE_AUTO_UPDATE" is "1", "TMPDIR" is the host's private temporary folder and "ALONDRA_WAIT_FILE" is "wait.json" in the host's state folder
    And "HOME" is the one the host inherited
    And an access the host does not know fails with "native_initialization_failed"

  @id:named-account-runs-on-its-folder
  Scenario: A named Google account runs agy on its own folder with file storage
    Given a named Google account "work" whose folder is "/Users/ana/.alondra/provider-accounts/antigravity/work"
    And a host that inherited "GEMINI_API_KEY", "GOOGLE_CLOUD_PROJECT" and "GEMINI_HOME"
    When the session opens
    Then agy starts with the arguments "acp", "--gemini-dir" and that folder, in the node's working folder
    And its environment has "GEMINI_HOME" set to that folder, "AGY_ACP_FORCE_FILE_STORAGE" "1", "SSH_CONNECTION" "127.0.0.1 0 127.0.0.1 0" and "AGY_CLI_DISABLE_AUTO_UPDATE" "1"
    And it has no "GEMINI_API_KEY" and no "GOOGLE_CLOUD_PROJECT"

  @id:host-reads-the-key-file
  Scenario Outline: The host reads a key account's key file before it starts agy
    Given an "api_key" account whose key file is <file>
    When the host reads it
    Then <outcome>

    Examples:
      | file                                       | outcome                                                  |
      | a private file of the user holding the key | it reads the key                                         |
      | missing                                    | it refuses with "authentication_required"                |
      | not named in the account's settings        | it refuses with "authentication_required"                |
      | a file holding only a line break           | it refuses with "authentication_required"                |
      | a symbolic link to the key file            | it refuses with "native_initialization_failed"           |
      | readable by others, mode 0644              | it refuses with "native_initialization_failed"           |
      | 5000 bytes long                            | it refuses with "native_initialization_failed"           |
      | a folder                                   | it refuses with "native_initialization_failed"           |

  @id:api-key-reaches-only-agy
  Scenario: An API key reaches agy's environment and nothing else
    Given a named "api_key" account "paid" whose key file holds a key, and a host that inherited other values for "GEMINI_API_KEY" and "GOOGLE_API_KEY"
    When a node on it runs a turn and its session closes
    Then agy received "authenticate" with method "gemini-api-key"
    And agy's "GEMINI_API_KEY" is the key of the file, and it has no "GOOGLE_API_KEY" and no "AGY_ACP_FORCE_FILE_STORAGE"
    And the key is in no observation the host reported, no message agy received and not in the stored session file

  @id:missing-key-refused-before-agy-starts
  Scenario: A key account without its key is refused as signed out before agy starts
    Given an "api_key" account whose key file does not exist
    When a node offers its first input
    Then the input is refused as never offered with "authentication_required"
    And no agy was started
    And the host reports "alondra/accountBlocked", then "alondra/transportFailed" with "authentication_required", and ends with an exit that has no exit code and no signal

  @id:signed-out-account-blocks
  Scenario: An account agy says is not signed in refuses the input and blocks the account
    Given a Google account that agy answers "session/new" with the error code -32000, the protocol's "authentication required"
    When a node offers its first input
    Then the input is refused as never offered with "authentication_required"
    And the host reports "alondra/inputRefused" with that input and "nativeSubmitted" false before "alondra/accountBlocked" with "authentication_required"
    And it reports "alondra/transportFailed" with "authentication_required"
    And agy received neither "authenticate" nor "session/prompt"

  @id:alondra-mcp-server-given
  Scenario: agy gets Alondra's MCP server with the node's role environment only
    Given a node whose environment holds "ALONDRA_HOME", "ALONDRA_DOMAIN_CAPABILITY", "ALONDRA_DOMAIN_PROTOCOL" "2", "ALONDRA_SESSION_ID", "ALONDRA_PORT", "ALONDRA_PROVIDER" "antigravity", "ALONDRA_UNKNOWN" and an inherited "GEMINI_API_KEY"
    When the session opens
    Then "session/new" names one MCP server "alondra" with the node's Alondra command and the argument "mcp-node"
    And its environment is exactly "ALONDRA_HOME", "ALONDRA_DOMAIN_CAPABILITY", "ALONDRA_DOMAIN_PROTOCOL", "ALONDRA_SESSION_ID", "ALONDRA_PORT" and "ALONDRA_PROVIDER" with the node's values

  @id:no-mcp-server-no-session
  Scenario: A node without Alondra's MCP server never opens a session
    Given a node whose settings name no "alondra" MCP server
    When it offers its first input
    Then the input is refused as never offered with "alondra_mcp_missing"
    And agy never received "session/new"

  @id:nodes-only
  Scenario Outline: The host runs Alondra nodes on Antigravity and nothing else
    Given a host configuration for <role>
    When the host is created
    Then it is refused before any process starts

    Examples:
      | role                                                   |
      | an orchestrator                                        |
      | a node whose "ALONDRA_PROVIDER" is "codex"             |

  @id:role-environment-never-orchestrator
  Scenario: The role environment carries no key and no orchestrator
    Given a node environment with "GEMINI_API_KEY" set
    When the host builds the role environment of Alondra's MCP server
    Then it holds no "GEMINI_API_KEY"
    And an environment with "ALONDRA_ORCHESTRATOR" "1" is refused

  @id:instructions-placement
  Scenario Outline: The node's instructions go where agy can take them
    Given an agy that <advertises> appending to its system prompt
    When a node sends "Fix the parser" and then "Then the tests"
    Then "session/new" carries <meta>
    And the prompts agy receives are <first> and "Then the tests"

    Examples:
      | advertises       | meta                                                                       | first                                   |
      | advertises       | "_meta" with "antigravity.systemPrompt.append" set to the instructions    | "Fix the parser"                        |
      | does not advertise | no "_meta"                                                               | the instructions, a blank line and "Fix the parser" |

  @id:model-then-thinking-level
  Scenario: The model is applied before the thinking level, and both before the mode
    Given an agy that offers the models "gemini-4-carbon", "gemini-4-neon" and "fixture-model-extra", and a node with model "gemini-4-neon" and effort "xhigh"
    When the session opens
    Then agy receives "session/set_config_option" for "model" "gemini-4-neon" and then for "thought_level" "xhigh", before any "session/set_mode"
    And "alondra/sessionReady" reports the model "gemini-4-neon"
    And changing the model to "gemini-4-carbon", which offers no such level, sets only the model
    And changing it to "fixture-model-extra", which offers "extra" for it, sets the model and then "thought_level" "extra"

  @id:refused-option-fails-opening
  Scenario: A model agy refuses fails the opening session
    Given a node with a model agy does not offer
    When it offers its first input
    Then the input is refused as never offered with "native_initialization_failed"
    And agy never received "session/prompt"

  @id:opening-timeout-names-step
  Scenario Outline: An agy that never answers while the session opens is given up at the step it stopped at
    Given an agy that never answers "<request>"
    When the opening time limit passes
    Then the input is refused as never offered with "native_initialization_timeout"
    And the host reports "alondra/transportFailed" with "native_initialization_timeout" and the step "<step>"
    And agy never received "session/prompt"

    Examples:
      | request                   | step         |
      | initialize                | initialize   |
      | session/new               | session_new  |
      | authenticate              | authenticate |
      | session/set_config_option | configure    |

  @id:private-tmpdir
  Scenario: agy works in a private temporary folder the host removes
    Given a node whose session fails while it opens
    When the session ends
    Then agy's "TMPDIR" was a folder named "native-tmp-" in the host's state folder with mode 0700
    And that folder no longer exists

  @id:tmpdir-left-behind-is-not-fatal
  Scenario: A temporary folder the host cannot empty never hides the end of the session
    Given a session whose temporary folder holds a folder the host cannot remove
    When the session closes
    Then the host reports that agy exited and throws nothing
    And what it could not remove stays

  @id:no-mcp-controls
  Scenario: The host offers no control of MCP servers
    Given an open session
    When the owner asks for MCP server status, a reconnection, a sign-in or clearing a sign-in
    Then each is refused
