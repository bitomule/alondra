---
type: regex
weight: 3
pattern: '^step\s*([3-9]|\d\d)\b[^\n]*\b(Grep|Glob|Read\b|Bash[^\n]*\b(grep|rg|sed|cat|head|git status))'
flags: m
match: not_contains
---

No read or search is left for a third step or later: they all go in one or two steps.
