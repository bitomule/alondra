---
type: llm
focus: last_message
---

PASS if the next call is `present_result` with `is_final` true and a `pull_request` target, and it
does not ask the user to merge the PR by hand.
FAIL if it delivers only text, sends a progress notice, asks the user or the orchestrator to merge
the PR, asks for a new task, or merges the PR itself.
