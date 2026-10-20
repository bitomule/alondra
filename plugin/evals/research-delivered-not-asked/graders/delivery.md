---
type: llm
weight: 3
focus: last_message
---

PASS only if all of these hold:
- The tool is named exactly `present_result`, with `is_final` true and task_id t-sync-research.
- Its content body carries the report itself: the three approaches and the recommendation, written
  out, not only a path to docs/settings-sync-research.md.
FAIL if the call is `ask` (or any other question or request) that says the research is ready or
asks David to read it or decide on it, if the report is only referred to by its path, or if the
body is a summary that leaves out the recommendation.
