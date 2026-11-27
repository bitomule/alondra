@spec:antigravity.models
Feature: The models and thinking levels of an Antigravity account
  An account's catalog is what its agy offers when a session opens: the
  values of the session's "model" select option, each with the values of its
  "thought_level" option. Alondra calls a thinking level by the effort of the
  same name, except that agy's "extra" is Alondra's "xhigh". A level outside
  that table keeps its own id, and a level the model does not offer is never
  sent.

  @id:catalog-from-select-options
  Scenario: The catalog is read from the session's select options
    Given a session whose "model" option offers "gemini-4-neon", "gemini-4-carbon" and a value holding a control character, with "gemini-4-carbon" selected
    And whose "thought_level" option offers "low", "medium" and "extra", with "medium" selected
    When its catalog is read
    Then the models are "gemini-4-neon" "Gemini 4 Neon" and "gemini-4-carbon" "Gemini 4 Carbon"
    And "gemini-4-carbon" is the default model, with the efforts "low", "medium" and "xhigh" and the default effort "medium"
    And a session with no "mode" option reads none

  @id:each-model-has-its-own-levels
  Scenario: The check reads every model's own thinking levels
    Given a signed-in agy whose default model is "gemini-4-carbon" and that also offers "gemini-4-neon"
    When the account is checked
    Then "gemini-4-neon" "Gemini 4 Neon" offers "low", "medium", "high" and "xhigh"
    And "gemini-4-carbon" offers "low", "medium" and "high"

  @id:levels-and-efforts
  Scenario Outline: The host sends the thinking level of the effort, and "extra" for "xhigh"
    Given a node asking for the effort "xhigh"
    When the host applies a model that offers <offered>
    Then the level sent is <level>

    Examples:
      | offered                     | level   |
      | "xhigh"                     | "xhigh" |
      | "extra" and not "xhigh"     | "extra" |
      | neither "xhigh" nor "extra" | none    |

  @id:levels-read-as-efforts
  Scenario Outline: A level read from agy is shown as its effort
    Given agy offers the level "<level>"
    When the catalog shows it
    Then it shows the effort "<effort>"

    Examples:
      | level      | effort     |
      | extra      | xhigh      |
      | xhigh      | xhigh      |
      | minimal    | minimal    |
      | ultra-deep | ultra-deep |

  @id:model-family
  Scenario Outline: A model belongs to the family its id names
    Given the model "<model>"
    When its family is read
    Then it is "<family>"

    Examples:
      | model                       | family      |
      | gemini-4-neon               | gemini-4    |
      | Gemini-3.8-Flash            | gemini-3.8  |
      | gemini-4                    | gemini-4    |
      | claude-opus                 | claude-opus |
      | antigravity/gemini-4-carbon | gemini-4    |

  @id:shared-model-name-is-ambiguous
  Scenario: A model name Antigravity shares with Claude names both providers
    Given Claude offers "opus" and an Antigravity account offers "opus" and "gemini-4"
    When the providers offering a model are asked for
    Then "opus" is offered by Claude and Antigravity, and "gemini-4" by Antigravity alone

  @id:request-offers-antigravity-accounts
  Scenario: A request is offered the Antigravity accounts beside the Claude ones
    Given Claude and Antigravity connected, with the accounts "Work" on Claude and "Lab" on Antigravity
    When a request's account choice is listed
    Then it offers "claude/work" and "antigravity/lab"
