---
name: alondra-domain-v2
description: Use Alondra domain protocol 2 to work on an assigned Task, ask the user, deliver a result, register a PR, or claim an authorized merge.
---

# Alondra node protocol 2

The Alondra MCP server is the channel to the user. The daemon owns the durable domain state;
this Claude conversation is working context. Use the cursor supplied with the durable inbox
batch before a domain mutation.
Each `domain_command` needs `protocol_version: 2`, the current `data_epoch` and
`expected_revision`, and a unique `command_id`. Retry an uncertain call with exactly the same
payload and ID. Obtain a fresh cursor after a conflict. A missing producer capability means this
session uses an old plugin: stop domain writes and ask the orchestrator to resume or relaunch it.
A session ID, PID, tool name or process ancestry never authorizes a command.

## Task and outputs

Work only on your assigned Task. Name its `task_id` explicitly in every command. If you work
on multiple Tasks, use a separate worktree and subagent for each, with Alondra's task binding.
One implementation Task should fit one PR. Reports, documents and answers need no PR merely to
satisfy that rule. Outputs remain linked to their Request and Task in the domain.

Use `domain_command` with the typed `command.kind`:

- `publish_information` to tell the user or orchestrator a finding. This does not complete work.
- `ask_question` or `request_action` for a decision or outside action. Include the whole context,
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

A User authorization names one PR, one exact head, one method and a domain revision. Until the
v2 runner provides an exact authorization read adapter, do not execute a merge. Once available,
the assigned Agent claims it through
`domain_command` with `kind: claim_merge_authorization`; the daemon
checks that authorization is still valid, the Task is active, the head matches, and its stack
base is ready. After a successful claim, execute `gh pr merge --match-head-commit <approved SHA>`
with the authorized method. Preserve dependency guards and retarget a stacked PR only after its
upstream merge is confirmed. If the command times out or returns an ambiguous result, inspect
GitHub before any retry and record the observed result. Cancellation after an external merge
starts cannot undo it.

Technical permission prompts still work through their separate channel. Do not turn a permission
prompt, hook text, or end of a Claude invocation into a domain Question or completed Task.

## Testing

Run probes against `make e2e-sandbox`, never the user's live daemon. Name probe nodes `test/...`
and remove them and their worktrees when finished.
