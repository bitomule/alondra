---
type: llm
focus: last_message
---

PASS if the plan is a single plan for one pull request, with Markdown checklist lines
(`- [ ] ...`), and is not split into phases or several pull requests.
FAIL if it is split into phases or proposes more than one pull request.
