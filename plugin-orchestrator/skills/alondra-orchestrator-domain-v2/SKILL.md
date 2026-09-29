---
name: alondra-orchestrator-domain-v2
description: Coordinate Requests, Tasks, Agents and durable inbox batches through Alondra domain protocol 2.
---

# Alondra orchestrator protocol 2

The user supervises the fleet. Their questions and decisions are durable domain records. This
Claude conversation handles one inbox batch at a time; finishing the invocation never completes
a Task. Alondra will resume the conversation for later batches. Do not poll or keep a background
loop alive. Use the `alondra-orchestrator` MCP server's named operations.
The separate node MCP catalog has different methods. Neither process ancestry nor a client
supplied role grants domain authority.

Use the cursor supplied with the durable inbox batch before acting. A mutation supplies
`data_epoch`, `expected_revision` and a stable unique `command_id`. Reuse the same arguments and ID
for an uncertain retry; obtain a fresh cursor after revision conflict. If this session lacks a
daemon-issued capability, stop task changes and
arrange a restart with the current plugin. Keep decisions and pending work in durable records,
not solely in this conversation.

## Requests and Tasks

Represent each user objective with a Request. Close an answered or withdrawn Request explicitly;
otherwise define the smallest useful set of cohesive Tasks. A code Task should fit one PR,
including implementation and validation. Do not split an answer, report or document merely to
force a PR. Give each Task a short title, complete brief, dependencies, assigned Agent and its
Request ID. Only explicitly assigned Agents may publish for or complete that Task. A new
Claude invocation, handoff to a live node, hook or sentinel observation never creates an
implicit Task or marks one complete.

Use `create_request` and `create_task` to record work; use `assign_task` and `set_dependencies` to
route it. The daemon's producer runner must make
that assignment deliverable before starting the Agent. Respect stack dependencies and machine
capacity. Keep separate worktrees for independent branches.

Preserve an `auto_merge` choice only when the user explicitly made it; it does not authorize the
orchestrator to merge a PR. A PR is associated only by the assigned Agent's `register_pr` tool,
with canonical repository, number, head and base. One Task owns at most one PR. The User reviews
a presented result and explicitly authorizes the exact PR head and merge method. The assigned
Agent claims and executes it with a remote head precondition. The daemon observes GitHub to
reconcile outcomes; it does not automatically merge domain v2 PRs. A document review approves
one immutable revision; reading a report is not acceptance.

## Inbox and messages

The durable batch may contain new Requests, Agent questions, reports, handoffs and uncertain
external effects. Nodes can ask the User directly. A question addressed to you needs an actual
answer; a handoff is work to route, not a question. Before retrying an
ambiguous merge or interrupted execution, reconcile its external effects. Technical permissions
stay in their own channel and must not be synthesized as business Questions.

For user-facing information use a Task or Request scoped `publish_information`. For a decision
use `ask` or `request_action` with complete context and options, each with a
`description` when its label alone does not say what it means. Route a Task result to the assigned
Agent, who presents it with `present_result`; after current User acceptance, that Agent calls
`complete_task`. For a Request with no Task, answer it with `close_request`. A report, invocation
end, merge observation or User read receipt alone cannot complete the Task.

## Clean finish

When work is integrated, make sure committed changes and durable artifacts are visible, then
remove finished worktrees and stale builds. Never delete uncommitted work from another session.
