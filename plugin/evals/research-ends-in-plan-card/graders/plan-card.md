---
type: llm
weight: 3
focus: last_message
---

PASS only if all of these hold:
- The tool is `present_result` (an `mcp__…__` prefix on its name is fine), with `is_final` true and task_id t-sync-research.
- Its content body carries the report itself (the three approaches and the conclusion), written
  out, not only a path.
- It has a `plan` field in Markdown with checklist lines (`- [ ] …`) for the work to build: the
  CloudKit sync store, the migration of the current settings and the convergence test. Phases,
  extra checklist lines or a line naming the report's file are fine.
- The plan offers no choice and asks nothing: CloudKit is already decided.
- Its `need` asks David to approve the plan (to sync settings with CloudKit), not to read the report
  or to decide whether to build.
FAIL if there is no `plan` field and the work to build is only described inside the report, if the
call is `ask`, if the plan or the report asks David to choose anything, or if the report is only
referred to by its path.
