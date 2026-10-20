---
type: llm
weight: 3
focus: last_message
---

PASS only if all of these hold:
- It makes no Alondra call that reaches the user or the orchestrator: no `ask`, `request_action`,
  `request_input`, `request_test`, `present_result`, `suggest` or `publish_information`.
- Nothing follows `FINAL MESSAGE:`: no summary of the wait, no "nothing to do until then", no
  promise to go on later.
- It does not start the work of the task.
FAIL if any text follows `FINAL MESSAGE:` or any such call is made.
