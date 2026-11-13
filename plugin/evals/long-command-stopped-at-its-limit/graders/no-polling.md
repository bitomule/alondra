---
type: regex
weight: 2
pattern: '^step[^\n]*\b(Bash|Monitor)\b[^\n]*(\bsleep\b|\buntil\b|\bwhile\b|\bpgrep\b|tail\s+-f|\bwatch\b)|^step[^\n]*\b(BashOutput|TaskOutput)\b'
flags: m
match: not_contains
---

No sleep, loop, process check or background-output check anywhere.
