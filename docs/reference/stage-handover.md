# The stage handover

What a new agent of a task in stages starts from, and the budget it must fit. The specs under
`specs/stage-handover/` are the source of truth; where this page disagrees with one, the spec wins.

## What a new stage starts from
<!-- section: stage-handover.overview explains: stage-handover.packet, stage-handover.budget, boundary:stage-handover, decision:a-stage-handover-references-the-plan-and-the-specs -->

A task in stages changes agent when its author's plan is approved, when the implementer is ready
for validation, and when the validator sends the work back. The new agent never sees the
conversation of the one before it. It starts from a short packet Alondra writes for its role.

The packet holds what the role does, the worktree, branch and pull requests, and the validation
that concerns its role. An implementer the validator sent back gets every failed validation with
the validator's reasoning and findings, whole: they are why it was started.

Everything else goes by reference, to files the agent reads when it needs them. The approved plan
is written to `files/tasks/<task>/approved-plan.md` in Alondra's home; when the task implements
only some points of a plan in phases, the packet still says which ones. The task's record (the
original request, every message and answer of the user, the decisions on its cards and the
earlier work it builds on) is written to `files/tasks/<task>/task-records.md`. The approved specs
go by their paths in the worktree, where Alondra already writes them; a project that keeps its
specs out of the repository gets them under `files/tasks/<task>/specs/`. The task's folder goes
with the task when it closes.

The packet must fit the model the new agent starts on. When the account's catalog gives that
model's window, the budget is the window less 4,096 tokens for its answer and less the message
that continues the task. When it does not, as for Claude, Codex and Antigravity today, the
budget is 100,000 tokens. Tokens are estimated as one per three bytes of text.

The packet and its budget are worked out from the domain's state and the paths their caller
gives them; writing the files is the caller's job.
