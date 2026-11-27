@spec:antigravity.usage
@decision:antigravity-quota-holds-its-group
Feature: The usage of an Antigravity account
  Alondra reads an account's usage by asking agy itself for its "/usage"
  report, never by calling Google. The report groups quota pools; each pool
  is one window named "antigravity:<group>:<pool>". One pool that runs out
  leaves the others working: the account stops taking work only when every
  pool ran out, and is back at the soonest reset. Accounts billed per use
  (access "api_key" or "cloud_project") read no usage.

  @id:usage-read-through-agy
  Scenario: The usage reading asks agy for its report and tells a refusal apart
    Given a signed-in Google account
    When its usage is read
    Then agy is run with the arguments "-p", "/usage", "--output-format", "json"
    And a report with status "SUCCESS" and three buckets in two pools reads as two windows, complete
    And a report with status "ERROR" reads as refused, not as a failure
    And a report agy ends with exit code 2 and no data fails

  @id:one-window-per-pool
  Scenario: Each pool is one window
    Given a report whose group "All Models" holds "gemini-4-neon-high" and "gemini-4-neon-low" with 0.25 left until "2030-01-01T00:00:00Z", and "claude-opus-5.5-max" with nothing left until "2030-01-02T00:00:00Z"
    When it is read
    Then the windows are "antigravity:all-models:gemini-4-neon-high", 75% used and resetting at "2030-01-01T00:00:00Z", and "antigravity:all-models:claude-opus-5.5-max", run out
    And a reset at "1970-01-01T00:00:00Z" means no reset is pending

  @id:colliding-pool-names
  Scenario: Pools whose names collide keep a window each
    Given a group holding "claude-opus-4-5-thinking-max" with half left and "claude-opus-4-5-thinking-low" with nothing left
    When it is read
    Then it reads as two windows, one of them run out

  @id:account-stops-when-every-pool-ran-out
  Scenario: The account stops only once every pool ran out, and is back at the soonest reset
    Given a reading where one pool has room and another ran out
    When the account is asked whether it has room
    Then it has room and no return time is given
    And once every pool ran out it has none, and it is back at "2030-01-01T00:00:00Z", the soonest reset

  @id:reset-passed-has-room
  Scenario: A pool whose reset passed has room again
    Given a pool of Gemini with room left that resets at "2030-01-01T00:00:00Z" and a pool of Claude that ran out until "2030-01-02T00:00:00Z"
    When the account is asked at "2030-01-01T12:00:00Z" whether it has room
    Then it has room

  @id:utc-times
  Scenario Outline: Reset times are UTC instants read to the millisecond
    Given the time "<text>"
    When it is read
    Then it is <result>

    Examples:
      | text                           | result        |
      | 1970-01-01T00:00:00Z           | 0             |
      | 2026-11-22T10:00:00.5Z         | 1795341600500 |
      | 2026-11-22T10:00:00.5+00:00    | 1795341600500 |
      | 2026-13-01T00:00:00Z           | refused       |
      | 2026-11-22 10:00:00            | refused       |
      | 2026-11-22T-1:00:00Z           | refused       |
      | 99999999999999-01-01T00:00:00Z | refused       |
      | 1969-12-31T23:59:59Z           | refused       |

  @id:checked-account-records-its-usage
  Scenario: A checked Google account records one window per pool, and an API key account none
    Given a signed-in default account whose usage has a pool with room and a pool that ran out, and a named "api_key" account with a key
    When the accounts are checked
    Then the default account's usage has two windows and still has room
    And the "api_key" account has no usage reading

  @id:account-with-room-keeps-the-mac-working
  Scenario: Antigravity accounts with room keep the Mac working while Claude waits
    Given a signed-in Claude account at its limit for an hour, a signed-in default Antigravity account and a named one billed per use
    When the Mac decides whether to pause for usage
    Then it does not pause: the Antigravity accounts take the work

  @id:usage-gauge-shows-antigravity
  Scenario: Antigravity drives the usage gauge with the other providers
    Given Claude, Codex and Antigravity accounts
    When the usage gauge is built
    Then it shows Claude, Codex and Antigravity, in that order
