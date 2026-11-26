---
type: llm
weight: 3
focus: last_message
---

PASS only if all of these hold:
- The tool is `record_validation` (an `mcp__…__` prefix on its name is fine) for task t-refunds,
  subject `billing.partial/part`, check `adequate`, verdict `unmet`.
- Its `reasoning` says the test does not check the amount that comes back nor what is left.
FAIL if it records `met`, edits the test or the code, calls `end_validation` or `present_result`
before recording this check, or asks the user.
