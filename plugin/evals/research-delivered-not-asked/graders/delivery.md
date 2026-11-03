---
type: llm
weight: 3
focus: last_message
---

PASS only if all of these hold:
- The tool is `present_result` (an `mcp__…__` prefix on its name is fine), with `is_final` true and task_id t-sync-research.
- Its content body carries the report itself: the three approaches and the recommendation, written
  out, not only a path to docs/settings-sync-research.md.
A `plan` field with the work the recommendation proposes is right: the result is then a plan card
Alice approves, and a `need` that asks to approve that plan is right. A target or a line that also
names the file, next to the report written out in the body, is fine.
FAIL if a call with a `plan` has a `need` that asks to read the report or to decide whether to
build, if the call is `ask` (or any other question or request) that says the research is ready or
asks Alice to read it or decide on it, if the report is only referred to by its path, or if the
body is a summary that leaves out the recommendation.
