@spec:antigravity.accounts
Feature: Antigravity accounts and how each reaches Google
  An Antigravity account says how its agy reaches Google with its access:
  "google" (agy's own Google sign-in), "enterprise" (a Gemini Enterprise
  sign-in, with a project and a location), "api_key" (a Gemini API key) or
  "cloud_project" (a Google Cloud project, with an Agent Platform key or the
  Mac's application default credentials). The default account is agy's own
  sign-in in "~/.gemini"; every other account is named and has a private
  folder of its own, so its sign-in never mixes with the user's. Alondra
  never signs in for the user: it shows the command that does.

  @id:access-sets-its-own-environment
  Scenario: Every agy an account starts inherits no Google credential and gets only its account's own
    Given an inherited environment holding "GEMINI_API_KEY", "GOOGLE_API_KEY", "GOOGLE_APPLICATION_CREDENTIALS", "GOOGLE_CLOUD_PROJECT", "GEMINI_HOME" and other providers' variables
    When agy is started for an account of each access, as the default account and as a named account with and without a key
    Then none of the inherited values reaches it, and "HOME" and "LANG" do
    And "AGY_CLI_DISABLE_AUTO_UPDATE" is "1"
    And "GEMINI_HOME" is set only for a named account, to its folder
    And "AGY_ACP_FORCE_FILE_STORAGE" and "SSH_CONNECTION" "127.0.0.1 0 127.0.0.1 0" are set only for a named "google" or "enterprise" account
    And "GEMINI_API_KEY" is the account's key only for "api_key", and "GOOGLE_API_KEY" only for "cloud_project"
    And "AGY_ADC_AUTH" is set only for a "cloud_project" account without a key
    And "GOOGLE_CLOUD_QUOTA_PROJECT" and "GOOGLE_CLOUD_LOCATION" are set only for "enterprise" and "cloud_project"

  @id:service-account-file-only-with-adc
  Scenario: A cloud account names its service account file only when it signs in with application default credentials
    Given a "cloud_project" account whose credentials file is "/Users/ana/keys/sa.json"
    When agy is started for it without a key, and then with a key
    Then without a key "GOOGLE_APPLICATION_CREDENTIALS" is "/Users/ana/keys/sa.json"
    And with a key "GOOGLE_APPLICATION_CREDENTIALS" is not set
    And only "agy acp" takes the "--gemini-dir" argument

  @id:settings-refuse-what-the-access-does-not-take
  Scenario Outline: Settings that do not fit the access are refused
    Given an Antigravity account with access "<access>" and <setting>
    When its settings are checked
    Then they are <result>

    Examples:
      | access        | setting                                                             | result   |
      | google        | nothing else                                                        | accepted |
      | google        | a project                                                           | refused  |
      | api_key       | a project                                                           | refused  |
      | cloud_project | no project                                                          | refused  |
      | cloud_project | the project "demo; rm"                                              | refused  |
      | cloud_project | a project and the credentials file "relative.json"                  | refused  |
      | enterprise    | the credentials file "/Users/ana/sa.json"                           | refused  |
      | cloud_project | a project, a location and the credentials file "/Users/ana/sa.json" | accepted |

  @id:api-key-account-is-always-named
  Scenario: The default account is agy's own sign-in, never a key
    Given a fresh accounts file
    When an account "Studio" with access "api_key" is added
    Then it gets the id "studio", keeps its access through a rename, and writes no billing
    And an account "Personal" added first with no access gets the default id
    And an accounts file whose default Antigravity account has access "api_key" is refused
    And adding a Codex account with Antigravity settings is refused

  @id:pay-per-use-follows-the-access
  Scenario Outline: A key access is billed per use and reads no usage
    Given the Antigravity access "<access>"
    When its default billing and its usage reading are asked for
    Then it is <billing> and <usage> its usage

    Examples:
      | access        | billing           | usage       |
      | google        | on a subscription | reads       |
      | enterprise    | on a subscription | reads       |
      | api_key       | billed per use    | never reads |
      | cloud_project | billed per use    | never reads |

  @id:older-options-still-read
  @boundary:data
  Scenario: Options written before Antigravity existed still read
    Given an "options.toml" whose accounts section has no Antigravity account
    When it is read
    Then it reads with no Antigravity account
    And an accounts section with a "cloud_project" account and an "api_key" account reads back unchanged after it is written
    And an Antigravity account that holds a "key" is refused: a key never goes in "options.toml"

  @id:named-account-has-a-private-folder
  Scenario: A named account gets a private folder linked to the user's own configuration
    Given a named Antigravity account "work" and a "~/.gemini" holding "GEMINI.md", "skills", "config/mcp_config.json" and the sign-in "oauth_creds.json"
    When its folder is prepared
    Then "<Alondra home>/provider-accounts/antigravity/work", its parent and its "config" folder have mode 0700
    And "GEMINI.md", "skills" and "config/mcp_config.json" in it link to the user's own files
    And no "oauth_creds.json" is in it
    And preparing it again after the user adds "AGENTS.md" links that too
    And the default account gets no folder
    And removing the account removes its folder and never what its links point to

  @id:linked-account-folder-is-refused
  Scenario: A named account's folder that is a link is never used
    Given the folder of the account "work" is a symbolic link to another folder
    When the folder is prepared and a key is saved
    Then both are refused
    And removing the account removes the link and keeps the folder it pointed to

  @id:sign-in-command-shown
  Scenario Outline: Accounts show the command that signs each account in
    Given an Antigravity account with access "<access>" and folder <folder>
    When its sign-in command is asked for
    Then it is <command>

    Examples:
      | access        | folder                                          | command                                                                                                                                                                                                      |
      | google        | none                                            | "agy acp --login"                                                                                                                                                                                            |
      | google        | "~/.alondra/provider-accounts/antigravity/work" | "GEMINI_HOME=~/.alondra/provider-accounts/antigravity/work AGY_ACP_FORCE_FILE_STORAGE=1 SSH_CONNECTION='127.0.0.1 0 127.0.0.1 0' agy acp --gemini-dir ~/.alondra/provider-accounts/antigravity/work --login" |
      | api_key       | "/x"                                            | none                                                                                                                                                                                                         |
      | cloud_project | none, with the project "demo-project"           | "gcloud auth application-default login --project demo-project"                                                                                                                                               |

  @id:last-account-on
  Scenario: An Antigravity account can stay on as the only account that runs nodes
    Given Antigravity and Claude both connected, each with its default account on
    When the Claude account is turned off
    Then an account that runs nodes is still on

  @id:orchestrator-never-on-antigravity
  Scenario: The orchestrator never runs on an Antigravity account
    Given a signed-in Antigravity account
    When an account is chosen for the orchestrator
    Then the choice is refused with "account_unavailable"

  @id:node-runs-on-the-account-it-names
  Scenario: A node runs on the Antigravity account its task names
    Given a signed-in default Antigravity account whose catalog offers "fixture-gemini"
    When a task that names that account is launched
    Then it runs on that account with the model "fixture-gemini"
