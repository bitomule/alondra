@spec:antigravity.key-file
@decision:antigravity-key-in-a-private-file
Feature: An account's key lives in one private file, read by one rule
  A named Antigravity account that signs in with a key (access "api_key" or
  "cloud_project") keeps it in the file "gemini-api-key" of its own folder,
  "<Alondra home>/provider-accounts/antigravity/<account>/". Alondra writes
  the file whole, private to the user, and never writes or reads through a
  link. The daemon and the Antigravity host read it by the same rule, kept in
  one fixture both test against, so a file reads as the same key, or as no
  key, wherever it is read. The key never goes anywhere else on the Mac: not
  in "options.toml", the domain, the views, a reply, a receipt or another
  file.

  @id:written-whole-and-private
  Scenario: Saving a key replaces the file whole with a private one
    Given a named account "work" with access "api_key" and its folder prepared
    When the key "AIza-first" and then the key "AIza-second" are saved while the umask is 000
    Then the file "gemini-api-key" of its folder has mode 0600 and reads as "AIza-second"
    And no other file of the folder holds a copy of the key
    And clearing the key removes the file and any copy a write cut short left beside it

  @id:links-are-never-followed
  Scenario: A key file that is a link is never read or written through
    Given the account's "gemini-api-key" is a symbolic link to a file outside its folder holding "not a key"
    When the key is read and a new key is saved
    Then reading is refused, saving is refused, and the file outside still holds "not a key"

  @id:one-rule-for-daemon-and-host
  Scenario Outline: The daemon and the host read a key file the same way
    Given the key file "<case>" of the shared fixture, with mode <mode>
    When the daemon, the Mac's accounts and the Antigravity host read it
    Then each reads <result>

    Examples:
      | case                                           | mode | result         |
      | the file Alondra writes                        | 600  | the key        |
      | a file only its owner can read                 | 400  | the key        |
      | a file written by hand, ending in a line break | 600  | the key        |
      | white space around the key                     | 600  | the key        |
      | the longest key                                | 600  | the key        |
      | an empty file                                  | 600  | no key         |
      | a file of white space                          | 600  | no key         |
      | a file its group can read                      | 640  | a refused file |
      | a file anyone can read                         | 644  | a refused file |
      | a file others can write                        | 602  | a refused file |
      | a space inside the key                         | 600  | a refused file |
      | a control character inside the key             | 600  | a refused file |
      | a key one character too long                   | 600  | a refused file |
      | a file past the size Alondra reads             | 600  | a refused file |
      | a file that is not text                        | 600  | a refused file |
      | a file with a second link                      | 600  | a refused file |

  @id:refused-file-reads-as-no-key
  Scenario: Only a file read as a key says that a key is set
    Given each key file of the shared fixture written in a named account's folder
    When the account says whether a key is set
    Then it says so only for the files that read as a key

  @id:same-credential-variables
  Scenario: The daemon and the host strip the same inherited credentials
    Given the list of credential variables in the shared fixture
    When the daemon's list and the Antigravity host's list are compared with it
    Then all three hold exactly "GEMINI_API_KEY", "GOOGLE_API_KEY", "GOOGLE_APPLICATION_CREDENTIALS", "GOOGLE_CLOUD_PROJECT", "GOOGLE_CLOUD_LOCATION", "GOOGLE_CLOUD_QUOTA_PROJECT", "GOOGLE_GENAI_USE_VERTEXAI", "AGY_ACP_FORCE_FILE_STORAGE", "AGY_ADC_AUTH", "GOOGLE_GEMINI_BASE_URL", "GEMINI_HOME" and "SSH_CONNECTION"

  @id:never-printed
  Scenario: A key never prints
    Given a launch of agy for an "api_key" account whose key is "AIza-secret", and a secret holding "AIza-visible"
    When each is printed for a log
    Then the launch's text names "GEMINI_API_KEY" and holds no "AIza-secret"
    And the secret's text holds no "AIza"

  @id:key-reaches-only-its-agy
  Scenario: A saved key reaches only its own account's agy
    Given a named account "studio" with access "api_key" whose key is saved through the daemon's route
    When the account is checked and the domain, the accounts screen, the reply, the receipt and every file under Alondra's home and the test's folders are read
    Then every agy started with the key had "GEMINI_HOME" set to the account's folder and "GEMINI_API_KEY" set to the key
    And the only file that holds the key is the account's "gemini-api-key"
    And neither the domain, the accounts screen, the reply nor the receipt holds it
