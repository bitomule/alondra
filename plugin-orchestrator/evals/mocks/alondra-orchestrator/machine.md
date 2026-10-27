---
type: fixed
---

Available models: model | efforts | account margin | sheet: cost, good for, caution | measured: tasks, % done first time, mean cost; kinds with 3+
claude/opus | low,medium,high | Personal 64% | high cost, feature/refactor/design/review/research/migration, Priciest Claude tier; wasted on mechanical work | 40: 80% $3.10; feature 14: 86% $3.40; fix 9: 78% $2.20
claude/sonnet | low,medium,high | Personal 64% | medium cost, feature/fix/refactor/tests/docs, Below Opus on TB4 (61.8% vs 64.8%) | 31: 74% $1.10; docs 12: 92% $0.80; feature 8: 50% $1.60
codex/gpt-6.1-sol | low,medium,high | Codex 88% | medium cost, fix/tests/migration/refactor/review/docs, Below the Claude Code runs on TB4 (58.2%) | 6: 67% $1.40; fix 3: 67% $1.30

{"model_evidence": {"claude/opus": "TB4: Claude Code + Opus 5.5 64.8%, top of the cited runs. AIDev: Claude Code led features (72.6%) and docs (92.3%). Warp: Claude Code for large refactors and integrations", "claude/sonnet": "TB4: Claude Code + Sonnet 5.5 61.8%. AIDev: Claude Code led features (72.6%) and docs (92.3%)", "codex/gpt-6.1-sol": "TB4: Codex + GPT-6.1 Sol 58.2% at $1.92/task. AIDev: Codex accepted 59.6-88.6% across all 9 categories. Warp: Codex for mechanical migrations and test backfill"}, "machine": {"memory": "fine", "cpu": "fine", "nodes_working": 1, "node_limit": 4}, "usage": {"week": "41% used", "today": "under its slice"}}
