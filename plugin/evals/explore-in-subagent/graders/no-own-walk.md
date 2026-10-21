---
type: regex
weight: 3
pattern: '(^step\s*([4-9]|\d\d)\b[^\n]*\b(Grep|Glob)\b[\s\S]*){2}'
flags: m
match: not_contains
---

The node does not go on searching the code itself: after the subagents, at most one targeted search
next to reading the files they name.
