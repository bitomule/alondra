---
type: regex
weight: 3
pattern: '^step\s*[1-3]\b[^\n]*\bAgent\b'
flags: m
match: contains
---

The tracing goes to a subagent at once: an Agent call by step 3, after at most two reads of its own.
