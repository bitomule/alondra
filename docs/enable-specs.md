# Turn specs on in a project

Specs are Gherkin files that say what your project does, checked by the spec engine against your
tests. This guide takes a git repository from nothing to a validated spec with a diagram and a
mockup. The exact formats and every rule are in [Specs](architecture/specs.md).

## 1. Install the command

`brew install bitomule/tap/alondra` installs `alondra` and `alondra-spec`. `alondra-spec <command>`
is the same as `alondra spec <command>` without the rest of Alondra: use it in a continuous
integration run, on a Mac with Apple Silicon. Neither needs the Alondra service for these commands.
Releases published before `alondra-spec` was added have only `alondra`.

## 2. Turn specs on

`alondra.toml` at the root of the repository:

```toml
format = 1

[specs]
enabled = true
```

## 3. Describe one component

A component is a part of your project with a boundary: the files it owns and how it is tested.
`specs/billing/boundary.toml`:

```toml
id = "billing"
owns = ["src/billing/**"]
regenerates = ["src/billing/**"]

[[suite]]
id = "unit"
run = ["npm", "test", "--", "billing"]
tests = ["tests/billing/**"]
```

`run` is any command; it is executed without a shell from the root of the repository, and it
passes when it ends with 0.

## 4. Write a spec

`specs/billing/partial.feature`:

```gherkin
@spec:billing.partial
@diagram:refund-flow.mmd
Feature: Partial refunds
  A customer gets back any amount up to what is left of the order, and nothing more.

  @id:part-of-an-order @mockup:refund-sheet.png
  Scenario: Part of an order
    Given a paid order of 30 EUR
    When 10 EUR are refunded
    Then the customer receives 10 EUR
    And 20 EUR are left to refund

  @id:stands-alone @boundary:dependencies
  Scenario: Billing uses no other component
    Given the billing folder
    When its imports are listed
    Then none comes from another component
```

Every component needs one scenario tagged `@boundary:dependencies`.

## 5. Add the diagram and the mockup

Both live in the folder of the component, and a tag names them by their path from it.

- `specs/billing/refund-flow.mmd` is Mermaid text that starts with `flowchart`, `graph`,
  `sequenceDiagram`, `stateDiagram`, `stateDiagram-v2` or `classDiagram`, up to 64 KiB:

  ```
  flowchart LR
    paid[Paid order] --> refund{Up to what is left?}
    refund -->|yes| back[Money returned]
    refund -->|no| refused[Refused]
  ```

- `specs/billing/refund-sheet.png` is a PNG or JPEG of the screen, up to 2 MiB.

They illustrate the rules; the steps remain what a test checks. Changing a mockup makes the
validations of the scenario that cites it stale, and changing a diagram of the feature those of
the tests of every scenario of the spec. A picture no tag cites is a problem, so none goes stale
unnoticed.

## 6. Tie each scenario to a test

A comment above a test says which scenario it verifies:

```js
// spec: billing.partial/part-of-an-order
test("refunds part of an order", () => { /* … */ })
```

In a language with `#` comments, write `# spec: billing.partial/part-of-an-order`. A scenario a
test cannot check is tagged `@judged`, or `@judged:capture` or `@judged:video`.

## 7. Validate

```sh
alondra-spec validate        # what is missing: 0 clean, 1 pending, 2 unreadable
alondra-spec run billing     # runs the suites and records the ones that pass
alondra-spec record billing.partial/part-of-an-order --kind adequate --verdict met
alondra-spec validate
```

`record --kind adequate` says that someone read the test and it really checks its scenario; it is
a judgment, not a measurement. Commit `specs/`, ledger included: the next run of `validate` reads
it. In continuous integration run `alondra-spec validate`, which runs nothing and writes nothing,
and fails when something that was validated has changed. `alondra-spec run <component>` runs the
suites again where you want that proof.

## 8. Work with it in Alondra

Once `alondra.toml` is on the default branch, a task in this repository that changes behaviour
proposes its specs with its plan, and you approve them there
([Specs in a task](architecture/specs.md#specs-in-a-task)). Decisions and reference prose are two
more folders you can turn on, under `[decisions]` and `[reference]`.
