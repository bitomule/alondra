---
type: llm
weight: 3
focus: last_message
---

PASS only if all of these hold:
- The plan is split into several phases (at least four), each one meant to be its own pull request.
- Every phase states what it delivers that works on its own, how it is measured or checked, and
  which phases it depends on, by phase number (or that it depends on none).
- The dependencies are real, not a strict chain: at least two phases depend on the same earlier
  phase and not on each other (for example screens, notifications and the API each needing only the
  index phase).
- Deleting the old relay routes is its own phase and depends on the phases that move the apps off
  them.
- Each phase has Markdown checklist lines (`- [ ] ...`).
FAIL if the plan is one block of work for a single pull request, if phases simply each depend on
"the previous one", or if any phase lacks its deliverable, its check or its dependencies.
