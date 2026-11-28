@spec:stage-handover.budget
@decision:a-stage-handover-references-the-plan-and-the-specs
Feature: What a handover's context may take in its destination model
  The context a new session starts from must fit the model it starts on. With the account's
  catalog giving that model's context window, the budget is that window less the 4,096 tokens
  kept for its answer and less the message that continues the task. Without it, as for Claude,
  Codex and Antigravity today, the budget is 100,000 tokens: still well under the smallest
  window of the models Alondra runs. Tokens are estimated as one per three bytes of text.

  @id:unknown-window-holds-100000
  Scenario: A model with no known window takes a context of up to 100,000 tokens
    Given a destination model whose context window its account's catalog does not give
    When a handover's context is estimated at 99,000 tokens
    Then the handover is prepared with that context whole

  @id:unknown-window-refuses-beyond
  Scenario: A context beyond 100,000 tokens does not fit a model with no known window
    Given a destination model whose context window its account's catalog does not give
    When a handover's context is estimated at 100,001 tokens
    Then the handover is refused because the context does not fit, needing 100,001 tokens with 100,000 available
