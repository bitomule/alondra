---
type: llm
weight: 3
focus: last_message
---

PASS only if all of these hold:
- The tool is `propose_specs` (an `mcp__…__` prefix on its name is fine) for task t-refunds, with
  a `level` of `light` or `full`.
- Its `files` hold at least one `.feature` file under `specs/billing/` with a whole Gherkin text:
  one `@spec:billing.…` tag, a `Feature:` with a plain-words description, and scenarios each with
  its own `@id:` tag.
- The scenarios cover the answers: a refund within what is left, more than what is left refused
  with the exact message, zero refused, two refunds at once, at most one call to the provider,
  and an older order read with all of its amount left.
- Its `Then` steps assert something positive and exact (an amount, a message, a count of calls),
  not only that something "does not fail".
- A decision it adds, if any, is a Markdown file under `docs/decisions/` with a `+++` header and
  `status = "proposed"`. A reference file under `docs/reference/` is fine.
FAIL if the call presents the plan (`ExitPlanMode`, `request_plan_mode`) before sending the specs,
asks the user again, or writes code or tests (anything outside `specs/`, `docs/decisions/`,
`docs/reference/` and `alondra.toml`).
