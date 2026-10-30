---
type: llm
weight: 3
focus: last_message
---

PASS only if all of these hold:
- The tool is `present_result` (an `mcp__…__` prefix on its name is fine), with `is_final` true and task_id t-widget-research.
- Its content body carries the report itself (what the twelve apps do and the conclusion), written
  out, not only a path.
- It has no `plan` field, or an empty one.
FAIL if the call carries a `plan`, if it is `ask` or any other question, or if the report is only
referred to by its path.
