@spec:antigravity.apps
@decision:antigravity-key-in-a-private-file
Feature: The key of an Antigravity account from the Mac and iPhone apps
  A named account with access "api_key" or "cloud_project" shows a key field
  in the account's settings in the apps, the terminal and the CLI. The daemon
  says only whether a key is set, never the key. A key the user types goes
  once, as the text of the account's "account_credential" action, to the
  Mac, which writes it to the account's private file. Until the Mac answers,
  the device keeps the action encrypted with its other pending answers, and
  once the Mac answers nothing on the device holds the key. Over the relay
  the key travels only inside the envelope sealed for the Mac.

  @id:accounts-show-access-key-and-sign-in
  Scenario: The accounts screen shows an account's access and whether it has a key, never the key
    Given the Antigravity accounts "default" (google, signed in), "studio" (api_key with a key), "fresh" (api_key without one), "lab" (cloud_project without one) and "work-g" (enterprise, agy too old)
    When the accounts screen is shown in English and in Spanish
    Then "default" reads "Antigravity · Google account" with its pool "gemini-4-neon-high 40 %" and offers no key action
    And "studio" reads "Antigravity · Gemini API key · Key set" and "billed per use", and offers "Replace key" as a secret text field and "Remove key" with the option "clear" after a confirmation naming "Studio"
    And "fresh" reads "No key", offers "Set key" and not "Remove key", and its reason names "alondra accounts set-key antigravity/fresh"
    And the reason of "lab" names "gcloud auth application-default login --project demo-project" and that of "work-g" reads "Update Antigravity CLI with agy update"
    And the key field of "studio" is labelled "Gemini API key" with "Key set", that of "fresh" reads "No key", that of "lab" is labelled "Agent Platform key", and "default" and "work-g" have none
    And adding an Antigravity account offers "google", "enterprise" and "api_key"
    And the Spanish screen reads "Clave de API de Gemini", "Clave puesta", "Sin clave", "Cambiar clave", "Quitar clave", "Poner clave" and "Clave de Agent Platform"

  @id:key-views-match-goldens
  Scenario: The key fields the apps receive are the committed ones
    Given the accounts "studio" (api_key with a key), "fresh" (api_key without one) and "lab" (cloud_project without one)
    When the accounts screen is rendered for the iPhone and the Mac, in English and in Spanish
    Then each equals its committed golden "accounts-antigravity-ios", "accounts-antigravity-ios-es", "accounts-antigravity-mac" and "accounts-antigravity-mac-es"

  @id:route-saves-the-key-to-its-file-only
  Scenario: The daemon's route keeps a key in its file and its own agy only
    Given a named "api_key" account "studio" added through the daemon
    When its key is sent from the relay and then from the Mac with the "account_credential" action
    Then a key from the relay waits while a local change holds the account settings, and is applied after
    And the file "gemini-api-key" of "studio" holds the key with mode 0600 and the account has a key with credential generation 2
    And the reply, the action's receipt, the domain and the accounts screen never hold the key, and the receipt keeps a digest of the request
    And a repeat of the same action gets the same reply, and the same action with another key is refused with "idempotency_conflict" and leaves the file as it was
    And only the account checks whose agy runs with "GEMINI_HOME" set to the account's folder get the key, as "GEMINI_API_KEY"
    And no other file of the test's folders holds the key

  @id:route-removes-the-key
  Scenario: Removing a key through the route deletes its file
    Given the named "api_key" account "studio" with a key
    When the "account_credential" action is sent with the option "clear"
    Then it is applied, the key file is gone and the account has no key

  @id:route-refuses-a-key-for-the-default-account
  Scenario: The default account takes no key through the route
    Given the default Antigravity account
    When a key is sent for it with the "account_credential" action
    Then the action is rejected, the reply does not hold the key and no file of Alondra's holds it

  @id:daemon-says-whether-a-key-is-set
  Scenario: The apps show whether a key is set and never receive one
    Given the iPhone accounts screen with "studio", "fresh" and "lab"
    When the apps read the accounts
    Then their key states are "Key set", "No key" and "No key", in Spanish "Clave puesta", "Sin clave" and "Sin clave"
    And their labels are "Gemini API key", "Gemini API key" and "Agent Platform key"
    And a Codex account has no key field

  @id:field-starts-empty
  Scenario: The key field starts empty every time an account opens
    Given the account "studio" whose key is set
    When its settings open, a key is typed, the settings close and they open again
    Then the field is empty each time it opens and nothing was sent

  @id:key-sent-once-and-forgotten
  Scenario: Saving sends the key once in the action and forgets it on the screen
    Given the account "fresh" with the key typed with white space around it
    When the user saves it and the Mac answers "Key saved"
    Then the field and the action's text are empty as soon as the key is sent, and the account stays open showing "Key saved"
    And exactly one action went, "account_credential:fresh@…" with no option and the key, without its white space, as its text
    And nothing the screen keeps holds the key

  @id:remove-sends-clear-without-a-key
  Scenario: Removing a key sends the option "clear" and no key
    Given the account "studio" with something typed in its key field
    When the user removes its key and the Mac answers "Key removed"
    Then the action sent has the option "clear" and no text, the screen shows "Key removed" and the field is empty

  @id:refused-key-shows-the-reason
  Scenario: A refused key shows the daemon's reason and is not kept
    Given the account "fresh" with "not-a-key" typed
    When the user saves it and the Mac refuses it with "That is not a Gemini API key"
    Then the screen shows "That is not a Gemini API key" as a failure and the field is empty

  @id:no-key-field-sends-nothing
  Scenario: Nothing is sent from a screen without a key field
    Given the accounts screen with no account open
    When a key is typed and saved
    Then nothing is sent

  @id:older-daemon-has-no-key-field
  @boundary:data
  Scenario: An account from a daemon without key fields still reads
    Given an account of the wire with a "credential" and one without it
    When the apps decode them
    Then the first has the label "Gemini API key", "Key set" and set, and the second has no key field

  @id:provider-reads-from-the-wire
  Scenario: The apps read Antigravity as a provider of its own
    Given the providers "claude", "codex" and "antigravity" on the wire, and an Antigravity account "work" with a sign-in link
    When the apps decode them
    Then they are named "Claude", "Codex" and "Antigravity", an unknown provider is refused, and the account's link keeps the provider "antigravity"
    And the account's row is named "Antigravity" with the id "antigravity/work"

  @id:outbox-keeps-the-key-encrypted
  Scenario: A pending key is on the device only encrypted, and leaves with the answer
    Given a key sent as an "account_credential" action that the Mac has not answered yet
    When the device's pending answers are read from disk, then the Mac answers "Key saved" and the store opens again
    Then the pending answer still carries the key to send it again, and no byte of its files holds the key in clear
    And once answered nothing is left to send again and no file holds the key

  @id:relay-sees-only-the-sealed-envelope
  Scenario: The relay carries the key only inside the envelope sealed for the Mac
    Given an "account_credential" action carrying a key, sealed from the device for the Mac
    When the envelope is encoded for the relay and opened by the Mac
    Then what the relay carries never holds the key
    And the Mac finds the key once, as the action's text

  @id:printing-an-action-hides-its-text
  Scenario: Nothing that prints an action shows its text
    Given an action carrying a key as its text, a comment, a field and a content value
    When it is dumped, described or reflected
    Then none of those outputs shows the key
    And its dump shows "redacted" and the action id
    And the action itself still carries the key to send it

  @id:iphone-key-screens @judged:capture @evidence:apple/Testing/Snapshots/Tests/__Snapshots__/ClaudeAccountsSnapshotTests/test_antigravity*.png
  Scenario: The iPhone's key settings read well in both languages and appearances
    Given the iPhone's settings of an account with a key, one without, and one whose key was just saved and checked, in English and in Spanish
    When their captures are looked at
    Then each shows the key state, the secure field and its buttons, without text cut off or overlapping, and the Spanish one reads in Spanish

  @id:mac-key-screens @judged:capture @evidence:apple/Testing/MacSnapshots/Tests/__Snapshots__/MacWorkspaceSnapshotTests/test_settings_antigravityKey_*.png
  Scenario: The Mac's key settings read well in both languages and appearances
    Given the Mac's settings of an account with a key, one without, and one whose key was just saved and checked, in English and in Spanish
    When their captures are looked at
    Then each shows the key state, the secure field and its buttons, without text cut off or overlapping, and the Spanish one reads in Spanish
