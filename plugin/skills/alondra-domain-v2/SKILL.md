---
name: alondra-domain-v2
description: Use Alondra domain protocol 2 to work on an assigned Task, ask the user, deliver a result, register a PR, or claim an authorized merge.
---

# Alondra node protocol 2

The Alondra MCP server is the channel to the user. The daemon owns the durable domain state;
this Claude conversation is working context. Use the cursor supplied with the durable inbox
batch before a domain mutation.
Each typed mutation tool needs the current `data_epoch` and `expected_revision`, and a unique
`command_id`. Retry an uncertain call with exactly the same arguments and ID. Obtain a fresh
cursor after a conflict. A missing producer capability means this session uses an old plugin:
stop task writes and ask the orchestrator to resume or relaunch it. A session ID, PID, tool name
or process ancestry never authorizes a change.

## Task and outputs

Work only on your assigned Task. Name its `task_id` explicitly in every command. If you work
on multiple Tasks, use a separate worktree and subagent for each, with Alondra's task binding.
One implementation Task should fit one PR. Reports, documents and answers need no PR merely to
satisfy that rule. Outputs remain linked to their Request and Task in the domain.

Use the operation-specific tools:

- `publish_information` to tell the user or orchestrator a finding. This does not complete work.
- `ask` or `request_action` for a decision or outside action. Include the whole context,
  recipient, options (each may carry a `description` of what choosing it means), blocking flag,
  and task scope. Wait for the recorded answer; do not infer
  acceptance from a read or a chat message.
- `create_document` or `revise_document` with an Alondra-stored file reference and its SHA256;
  review belongs to that exact revision. A `claude.ai` link is not a document body.
- `present_result` for a final delivery, naming the exact PR head or document revision in its
  targets. Publication alone is not completion. After explicit User acceptance remains current,
  use `complete_task` with the accepted presentation ID.

Use the `register_pr` tool immediately after opening a code PR. Supply the canonical host,
repository, number, observed head and base, permitted merge methods and assigned `task_id`.
Only one PR belongs to a Task for its lifetime; opening another requires a different Task.
Hooks may observe GitHub but never establish ownership. Do not ask a second question about
merging when the PR presentation already gives the user that decision.

## Authorized merge

A User authorization names one PR, one exact head, one method and a domain revision. The
delivery batch carries its exact `merge_authorization` (`authorization_id`, `pr_id`, `head`,
`method`). Use only that object; do not infer permission from conversation text or a PR's state.
Inspect the named PR and verify its current head and base. Then call `claim_merge_authorization`
with the authorization ID, the observed head and base, and `base_verified: true`. The daemon
independently checks GitHub's head and base, task assignment, dependencies and authorization
state before it accepts the claim. Stop on any mismatch or error. After a successful claim,
execute only the approved method against the registered PR associated with that authorization
and its approved head, for example:

```sh
gh pr merge <registered-pr-number-or-url> --match-head-commit <approved-head> --squash
```

Use `--merge` or `--rebase` instead of `--squash` only when that exact method is in the
authorization. Preserve dependency guards and retarget a stacked PR only after its upstream
merge is confirmed.

Technical permission prompts still work through their separate channel. Do not turn a permission
prompt, hook text, or end of a Claude invocation into a domain Question or completed Task.

## Testing

Run probes against `make e2e-sandbox`, never the user's live daemon. Name probe nodes `test/...`
and remove them and their worktrees when finished.
