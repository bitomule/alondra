@spec:antigravity.key
@decision:antigravity-key-in-a-private-file
@decision:antigravity-key-change-reaches-running-nodes
Feature: Setting, replacing and removing an account's key
  The user gives a named "api_key" or "cloud_project" account its key from
  an app, the TUI or "alondra accounts set-key", and removes it the same way.
  The daemon writes it to the account's key file and records in the domain
  only that a key is set, as the next generation of the account's
  credentials. The key file never holds a key the domain did not record, and
  an account that stops taking a key, or is removed and added again, keeps no
  key behind.

  @id:default-account-keeps-no-key
  Scenario: The default account keeps no key
    Given the default Antigravity account
    When a key is written for it
    Then writing it is refused

  @id:invalid-key-is-refused
  Scenario Outline: Only a key is kept
    Given a named "api_key" account with the key "AIza-second" saved
    When <key> is saved
    Then saving is refused

    Examples:
      | key                 |
      | the key "has space" |
      | an empty key        |

  @id:domain-refusal-rolls-back
  Scenario: A key change the domain refuses leaves the folder as it was
    Given a named "api_key" account with no key
    When a key change the domain refuses is made: a first key, then a replacement after "AIza-recorded" was saved, then a removal
    Then no key is left after the refused first key
    And the file holds "AIza-recorded" after the refused replacement and after the refused removal
    And the account still says a key is set

  @id:access-change-drops-the-key
  Scenario Outline: An account that changes access keeps its key only while its access stays
    Given a named account with access "<from>" and a key saved
    When its access changes to "<to>" and the change is written
    Then the account <kept> its key

    Examples:
      | from          | to            | kept          |
      | api_key       | cloud_project | no longer has |
      | cloud_project | api_key       | no longer has |
      | api_key       | api_key       | still has     |
      | cloud_project | cloud_project | still has     |

  @id:key-goes-only-with-the-written-change
  Scenario: A key goes only once the change that drops it is written
    Given a named "api_key" account "studio" with a key and a named account "lab" an open task runs on
    When a change removes "lab" and moves "studio" to access "google"
    Then the change is refused because "lab" is in use, and "studio" keeps its key
    And without the open task, preparing the change keeps the key and cleaning up after it removes the key

  @id:re-added-account-starts-without-a-key
  Scenario: An account added again starts without the key a failed removal left
    Given the account "studio" was removed but its key file was left
    When an account named "Studio" is added again
    Then it takes the id "studio" back and has no key

  @id:removing-the-account-removes-its-key
  Scenario: Removing an account removes its key
    Given a named "api_key" account with a key saved
    When the account is removed
    Then its key file is gone

  @id:cli-names-the-account
  Scenario Outline: set-key names an Antigravity account by its id or its name
    Given the command "alondra accounts set-key" run with provider "<provider>" for "<account>"
    When the account it names is read
    Then it is the "<named>" account "<name>"

    Examples:
      | provider    | account            | named       | name      |
      | claude      | antigravity/studio | antigravity | studio    |
      | claude      | antigravity/My Lab | antigravity | My Lab    |
      | antigravity | Studio             | antigravity | Studio    |
      | claude      | nowhere/x          | claude      | nowhere/x |

  @id:cli-reads-the-key
  Scenario Outline: set-key asks for the key on a terminal only when asking is allowed
    Given "alondra accounts set-key" whose input <input> and asking <allowed>
    When it reads the key
    Then it <reads>

    Examples:
      | input         | allowed        | reads                                               |
      | is a terminal | is allowed     | asks for it on the terminal                         |
      | is a terminal | is not allowed | stops: it never waits on a terminal with --no-input |
      | is a pipe     | is not allowed | reads it from the pipe                              |
      | is a pipe     | is allowed     | reads it from the pipe                              |
