@spec:antigravity.boundary
@decision:antigravity-reaches-google-through-agy
Feature: What the Antigravity integration depends on
  The integration runs Alondra nodes on Antigravity accounts. It reaches
  Google only through the "agy" binary: its version, its ACP server and its
  "/usage" report, and Google's public Gemini API only to check a Gemini API
  key. It reaches the rest of Alondra through the provider port the engine
  drives, the host protocol every agent host speaks, and the accounts, usage
  and views every provider shares.

  @id:reaches-google-only-through-agy @judged @boundary:dependencies
  Scenario: Nothing in the integration calls a Google endpoint itself
    Given the code the component owns
    When every process it starts and every network call it makes is listed
    Then the only programs it starts are "agy" (as "agy --version", "agy acp" and "agy -p /usage") and the Antigravity host
    And the only network call it makes itself is the check of a Gemini API key against Google's public Gemini API

