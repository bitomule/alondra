---
type: llm
weight: 3
focus: last_message
---

PASS only if all of these hold:
- The tool is `request_spec_change` (an `mcp__…__` prefix on its name is fine) for task t-refunds.
- Its `reason` says in plain words what the approved specs miss (which card is refunded first
  when an order was paid with two cards) and why the work needs it.
Naming possible rules as examples for the author is fine. FAIL only if it edits or writes the
feature file, proposes specs or an amendment itself, presents a result, or states one order of the
cards as what it will implement.
