# The task flow

How a task in a project with specs gets its specs approved and validated, by one agent or in
stages, and how a task that continues another takes over its work. The specs under
`specs/task-flow/` are the source of truth; where this page disagrees with one, the spec wins.

## Specs in a task without stages
<!-- section: task-flow.specs-without-stages-overview explains: task-flow.specs-without-stages, decision:one-agent-may-validate-its-own-specs -->

A task without stages is the cheaper way to work: one agent plans, builds and checks it. In a
project with specs that agent may change specs too. It is told so when it starts: a spec, a
boundary or a decision changes only when the agent proposes it with propose_specs and presents a
plan, and the user approves that plan. The card of the plan shows the specs, the user edits them
there and approving the plan approves that exact set, as with the author of a task in stages.

If the agent wrote specs before proposing them, its delivery is refused with specs_differ and the
refusal says how to go on: propose the files as they are and present a plan. It all happens in
the same task and with the same agent; no new task is needed.

Once the specs are approved, the same agent validates its work: it calls ready_for_validation,
records each result with record_validation and delivers only when everything is validated, through
the same gate as a task in stages. What it finds unmet comes back to itself. The delivery card
says that the agent that did the work validated it; a task in stages says a separate agent did.

## A task that continues another
<!-- section: task-flow.continuation-overview explains: task-flow.continuation, decision:a-continuation-owns-the-open-pull-requests, decision:validations-carry-over-to-a-continuation -->

The orchestrator may create a task that continues another when the first cannot finish its work.
The new task takes the original's open worktrees, its checklist and its open pull requests in the
same step, so each pull request always has one live owner. From then on cancelling the original
raises no card and closes nothing on GitHub, the original cannot register the pull request again,
and it never gets a new agent while the task that continues it is not finished. The sweep that
looks for pull requests nobody owns counts the continuing task as the owner of the branch it
inherited. A continuation is refused while the original's delivery waits for the user.

The validations come along too. When the ledger of the continuing task's commit holds an entry
that a validator of the original recorded, or of any task before it in the chain of
continuations, with the same subject, fingerprint and verdict, that entry counts as recorded for
the continuing task. Work already validated on that commit is then delivered without validating it
again: asking for validation answers that there is nothing to validate, and presenting the result
is accepted. An entry whose verdict or fingerprint differs from what was recorded, or that only an
unrelated task recorded, is still refused. The check reads the original's records when the
delivery is checked and stores nothing new, so a continuation refused before this change delivers
once Alondra is updated.

## What the rules may reach
<!-- section: task-flow.boundary-overview explains: task-flow.boundary, boundary:task-flow -->

The component owns the domain's rules for stages, specs, validation and continuations. They decide
on the domain's state alone and read a worktree only through the spec engine and the domain's
ports, the only component they depend on being the spec engine. Their tests are in one file per
suite: the rules suite and the views suite.
