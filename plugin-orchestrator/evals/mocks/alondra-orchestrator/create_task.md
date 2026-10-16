---
type: fixed
expect:
  task_id: string
  request_id: string
  title: string
---

{"ok": true, "task_id": "{{input.task_id}}", "created": ["{{input.task_id}}"]}
