---
type: regex
weight: 3
pattern: '^step[^\n]*\b(Bash|Read|Grep|Monitor|BashOutput|TaskOutput)\b(?![^\n]*after)[\s\S]*end (my |the |this )?turn'
flags: mi
match: not_contains
---

No call looks at the log or the process before the node ends its turn; reading the log once after
the notification is fine.
