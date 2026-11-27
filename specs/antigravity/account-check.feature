@spec:antigravity.account-check
@decision:antigravity-one-version-rule
@decision:antigravity-refused-key-signs-the-account-out
Feature: Checking an Antigravity account
  Alondra checks each Antigravity account by talking to agy over ACP the way
  a node does, but without ever sending a prompt or starting a sign-in: it
  learns whether agy is installed and recent enough, whether it is
  Antigravity, whether the account is signed in, and which models it offers.
  A check is reused until it is due again, so a stable account starts no
  agy; a new key, a change of settings or a request to check runs it again.

  @id:no-prompt-no-sign-in
  Scenario: A signed-in Google account is checked without a prompt or a sign-in
    Given the default account with access "google" and an agy 1.5.14 that is signed in
    When the account is checked
    Then agy is asked exactly "initialize", "session/new", "session/set_config_option" and "session/close", in that order
    And the check reads version 1.5.14 and the account as signed in, with its catalog

  @id:states-of-a-check
  Scenario Outline: The check tells why an account cannot be used
    Given the default account with access "google" and <agy>
    When the account is checked
    Then the check reads <state>

    Examples:
      | agy                                      | state                                     |
      | an agy that is not signed in             | signed out                                |
      | an agy whose version is 1.1.22           | can't be checked: too old, version 1.1.22 |
      | an agy whose agent name is "Other Agent" | can't be checked: another agent           |
      | an agy that speaks protocol version 2    | can't be checked: another agent           |
      | no agy at the configured path            | can't be checked: not installed           |

  @id:key-account-signs-in-with-its-own-key
  Scenario: A key account is checked with its own key alone
    Given a named account with access "api_key" and the key "AIza-own", and an inherited environment holding another key
    When the account is checked
    Then agy runs as "acp --gemini-dir <folder>" with "GEMINI_HOME" set to the folder and "GEMINI_API_KEY" set to "AIza-own"
    And the inherited key does not reach it
    And agy is asked to authenticate with "gemini-api-key"
    And the check reads the account as signed in

  @id:only-sign-in-required-is-signed-out
  Scenario Outline: Only ACP's sign-in-required answer reads as signed out
    Given a named "api_key" account <key>, and an agy that answers "authenticate" with <answer>
    When the account is checked
    Then the check reads <state>

    Examples:
      | key         | answer                    | state                    |
      | with no key | the sign-in-required code | signed out               |
      | with a key  | another error code        | can't be checked: failed |

  @id:supported-versions
  Scenario Outline: agy 1.5.12 or later is supported
    Given an agy that prints the version "<printed>"
    When its version is read
    Then it is <result>

    Examples:
      | printed    | result     |
      | 1.5.12     | supported  |
      | 2.0.0      | supported  |
      | 1.4.99     | too old    |
      | 1.1.22     | too old    |
      | no version | unreadable |

  @id:startup-check-runs-no-agy
  Scenario: The check after startup starts no agy and holds no launch back
    Given a Mac with Antigravity connected and an "api_key" account "studio"
    When the daemon starts and runs its first account check
    Then no agy is started and launches may go
    And "studio" reads as not checked yet

  @id:key-account-without-key-starts-no-agy
  Scenario: An API key account without a key is signed out without starting agy
    Given a named account "studio" with access "api_key" and no key
    When the accounts are checked
    Then "studio" is signed out and no agy started for its folder

  @id:new-key-checks-again
  Scenario: Saving a key checks the account again
    Given the account "studio" was checked signed out for want of a key
    When its key is saved and the accounts are checked
    Then exactly one agy started for its folder, "studio" says a key is set and is billed per use
    And the default account is signed in with its catalog recorded

  @id:checked-account-is-not-checked-again
  Scenario: An account checked recently starts no agy
    Given every Antigravity account was checked and the round was recorded
    When the accounts are checked again
    Then no agy is started
    And a round the domain never recorded is run again in full the next time

  @id:check-now
  Scenario: Asking for a check checks every account at once
    Given every Antigravity account was checked recently, and agy now reports version 1.1.22 for "studio"
    When a check is asked for
    Then agy is started again and "studio" reads "Unavailable" with the reason too old

  @id:doctor-checks-agy
  Scenario Outline: The doctor checks agy once Antigravity is connected
    Given Antigravity is <connected> and agy <prints>
    When "alondra doctor" runs
    Then the check "antigravity" <result>

    Examples:
      | connected     | prints           | result                                                                          |
      | connected     | prints "1.5.14"  | passes with "1.5.14"                                                            |
      | connected     | prints "1.1.22"  | fails naming "1.5.12", with the fix "agy update" for the user to run            |
      | connected     | is not installed | fails, saying in Spanish to install Antigravity CLI ("Instala Antigravity CLI") |
      | not connected | is not installed | is not run                                                                      |
