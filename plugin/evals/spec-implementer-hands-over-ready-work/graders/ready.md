---
type: llm
weight: 3
focus: last_message
---

PASS only if the tool is `ready_for_validation` (an `mcp__…__` prefix on its name is fine) for task
t-refunds. FAIL if it calls `present_result`, `record_validation`, runs or records the ledger
itself, or asks the user whether to deliver.
