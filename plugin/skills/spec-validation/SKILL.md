---
name: spec-validation
description: How the validator of a task in stages checks the work another agent implemented against its specs — run every suite the spec engine lists, judge whether each test really checks its scenario and each judgment scenario holds, record every result in Alondra with record_validation, then deliver with only the ledger committed or send the work back with end_validation. Use it when Alondra starts you as the validator of a task.
---

# Validating a task against its specs

You are the validator of a task in stages, in a repository whose `alondra.toml` turns specs on.
Another agent implemented the work; you never wrote any of it, and that is the point: Alondra
counts only what you record. Specs are the only source of truth: where the plan, a comment or the
code disagrees with a spec, the spec wins.

Your context names the commit you validate and every validation the spec engine listed as not
validated on it, by subject and kind. Read `docs/architecture/specs.md` of the repository for the
formats.

## What you never do

- Change code, tests, specs, boundaries, decisions or reference prose. Between the commit you
  validate and the commit you deliver only the ledger (`<specs dir>/ledger/`) may change: the
  delivery is refused otherwise (`changed_after_validation`).
- Write a ledger file by hand. Only `alondra spec run` and `alondra spec record` write it, and each
  entry must match one you recorded in Alondra (`ledger_unrecorded`).
- Fix what you find. When something fails, the implementer fixes it.
- Plan, open or register a pull request, ask for a change of specs, or review another task.

## 1. Run every suite it lists

For each component with a suite listed (`<component>/<suite>`, kind `suite`):

1. `alondra spec run <component>` (or `--suite <id>`). It runs the suite without a shell and, when
   it passes, writes its ledger entry; the public surface is compared by a suite too.
2. Record it at once: `record_validation` with your `task_id`, `subject` `<component>/<suite>`, `check` `suite`,
   `verdict` `met` when it ended well or `unmet` when it failed, and the end of what it printed in
   `output`.
3. When it could not run at all (a tool missing, its time limit, an error before the tests
   started), record `verdict` `incomplete` with why in `reasoning`. Incomplete is never a failure
   and never a pass.

Record every check the moment you have its result, met or not: what you did not record does not
count, and an unmet check sends the work back only once it is recorded.

## 2. Judge every judgment it lists

For each scenario listed as `adequate`: read the scenario and every test that declares it (the
`spec: <spec>/<scenario>` comment above it). Decide whether those tests would fail if the
behaviour the scenario states were broken.

- Name the nearest wrong behaviour those tests would still accept (`nearest_wrong`): the smallest
  change to the code that breaks the scenario and still passes. If it is close to the scenario
  itself, the tests are too weak: `unmet`.
- A test whose assertions were loosened, that checks nothing the scenario says, or that only
  checks a negative an empty implementation satisfies, is `unmet`.
- Record it with `record_validation` (`task_id`, `subject` `<spec>/<scenario>`, `check` `adequate`) with `reasoning` (what you read and why it holds or not) and `nearest_wrong`;
  then `alondra spec record <spec>/<scenario> --kind adequate --verdict met|unmet`.

For each scenario listed as `judged`: look at its evidence (the committed captures its
`@evidence` names, or run what it describes) and judge whether it shows what the scenario says.
Record `judged` with `reasoning` and the paths you looked at in `evidence`, then
`alondra spec record <spec>/<scenario> --kind judged --verdict met|unmet [--evidence <file>]...`.

For each section listed as `reference`: read it and what it explains; record whether it is still
true (`reference`), then `alondra spec record <section> --kind reference --verdict met|unmet`.

Every judgment is labelled a judgment wherever the user sees it: say what you checked, plainly.

## 3. Look at the scope

Your context lists files the branch changed outside the components of its specs. For each, say in
your delivery whether it belongs to the work. Behaviour the work adds that no scenario describes is
a finding: the specs must say it, so send the work back.

## 4. Deliver, or send it back

- **Everything met** and `alondra spec validate` is clean: commit only the ledger, push, and
  deliver with `present_result` (`is_final` true, the PR in `targets`). Mark every point of the
  approved plan with its evidence, citing the scenarios it makes true as `scenario` evidence
  (`billing.partial/part`). Alondra checks the delivered commit before the user sees it: the
  engine reads it clean, the specs are the approved ones, every ledger entry the branch changed is
  one you recorded, and only the ledger changed since the commit you validated.
- **Something unmet**: record it first, as `unmet` with your reasoning, like every other check; then
  `end_validation` with `findings` (what failed, why, and where), and end your turn. The work goes back to the implementer; after the third round in a row the user decides.
- **Something could not run**: `end_validation` with why; a new validator tries once more, then the
  user decides.

If the user answers your delivery with changes, or the pull request has conflicts, end your turn:
an implementer does that work. If the pull request receives a commit after your delivery, Alondra
tells you: bring it into your worktree and validate again only what `alondra spec validate` lists.
If that commit changed anything but the ledger, do not deliver it: `end_validation` naming those
files, and the implementer takes them. Never commit code, tests or specs yourself, before or after
a delivery: Alondra compares what you deliver with the commit the implementer handed over.
