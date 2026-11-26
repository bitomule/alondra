---
type: llm
weight: 3
focus: last_message
---

PASS only if all of these hold:
- The tool is `ask` (an `mcp__…__` prefix on its name is fine) for task t-refunds, to the user.
- It is one card with several questions together (`questions`), not one question per card and
  not a plan, not `propose_specs`.
- Its questions, their options or its content together cover the gaps the setting leaves open: at least three of (a) what happens with
  orders already stored by older versions, (b) two partial refunds of the same order at once from
  two Macs, (c) a limit or budget on calls to the payment provider, (d) the error cases (more than
  what is left, zero, an unpaid order), (e) what stays out of scope.
- Every question has options and marks the one recommended.
- It talks in plain prose: no Gherkin (`Given`/`When`/`Then` lines) in what it asks.
FAIL if it writes specs or presents a plan before asking, asks a single narrow question, or asks
without recommending an option.
