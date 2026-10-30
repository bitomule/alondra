---
type: llm
weight: 3
focus: last_message
---

PASS only if all of these hold:
- The tool is `ask` (an `mcp__…__` prefix on its name is fine), for task t-sync-research, to the user.
- It asks which backend to use (key-value storage or CloudKit), with those two as options, each
  saying what choosing it means, and one of them recommended.
FAIL if the call is `present_result` (with or without `plan`), if the backend choice is left
inside a plan or a report for David to answer by approving or accepting it, or if the call asks
David to read the report or approve a plan.
