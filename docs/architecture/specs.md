# Specs

How a project keeps its specs, what the spec engine checks and what it can and cannot guarantee.
The behaviour is specified in `specs/spec-engine/`; this page is the map and the exact formats.
Where it disagrees with a spec, the spec wins.

## What is guaranteed

The guarantee reaches as far as a scenario with a test reaches, whatever the test measures:
behaviour, a performance budget, a concurrency rule.

- Every scenario is declared by at least one test or is marked as a judgment.
- A scenario passes when every suite that holds one of its tests ran well on the current
  fingerprint of its component.
- A judgment (that a test really checks its scenario, that a capture or a video shows what the
  scenario says, that a reference section is still true) is a verdict someone recorded for the
  current fingerprint. It is evidence of a judgment, never of a fact.
- Nothing is guaranteed about what no test looks at. Code written again from the specs is equal to
  the old code only as far as the scenarios and their tests can tell.
- A ledger file can be written by hand. The engine does not know who recorded a verdict: its
  caller decides who may, and a continuous integration run executes the suites again.

## Three levels

| Level | What | Rules? |
|---|---|---|
| Visual explanation | Diagrams made when they help | No. Never stored as truth |
| Reference prose | `docs/reference/*.md`, each section anchored to what it explains | No. A spec wins over it |
| Specs | Feature files, boundaries, decisions | Yes. The only input of a regeneration |

## Files

```
alondra.toml                       settings
specs/<component>/boundary.toml    the boundary of a component
specs/<component>/*.feature        its specs
specs/ledger/...                   what was validated
docs/decisions/<id>.md             lasting decisions
docs/reference/*.md                reference prose
```

### `alondra.toml`

```toml
format = 1

[specs]
enabled = true      # default false
committed = true    # default true
dir = "specs"       # default "specs"

[decisions]
enabled = true      # default false
dir = "docs/decisions"

[reference]
enabled = true      # default false
dir = "docs/reference"
```

No other key is accepted. Folders are relative paths inside the repository. With specs disabled
nothing is read. With `committed = false` the specs live outside the repository and a marker with
no spec is not a problem.

### Limits and unreadable files

Every file the engine parses has a size limit; a larger one is refused with `too_large` naming the
file, before it is parsed.

| File | Limit in bytes |
|---|---|
| `alondra.toml`, `boundary.toml`, a ledger file | 65536 |
| A feature file, a decision, a reference document | 262144 |
| JUnit results handed to `read_results` | 16777216 |

A listed file the engine needs (the settings, a boundary, a feature file, a decision, a reference
document, a ledger file or a test file of a suite) that the caller cannot read is refused with
`unreadable_file` naming it. An owned file, an input or a piece of evidence that cannot be read
counts as absent in its fingerprint.

### Feature files

One spec per file. Tags are words that start with `@`, on the lines above `Feature:` or above a
scenario, one or several per line.

| Where | Tag | Meaning |
|---|---|---|
| Feature | `@spec:<id>` | The identifier of the spec. Exactly one |
| Feature | `@decision:<id>` | A decision the spec rests on |
| Scenario | `@id:<id>` | The identifier of the scenario. Exactly one |
| Scenario | `@judged`, `@judged:capture`, `@judged:video` | A judgment, and the evidence it needs |
| Scenario | `@evidence:<pattern>` | Committed captures the judgment looks at. Only with `@judged:capture` |
| Scenario | `@boundary:surface`, `@boundary:data`, `@boundary:dependencies` | The scenario states a promise of the boundary |
| Scenario | `@reserved` | Kept out of the packet handed to an implementer |

Identifiers use lowercase letters, digits, dots and hyphens and start with a letter or a digit.
A scenario is named `<spec>/<scenario>`.

Lines: `Feature: <title>`, free description lines, `Background:`, `Scenario: <title>` or
`Scenario Outline: <title>`, steps that start with `Given`, `When`, `Then`, `And` or `But`,
table rows between `|`, doc strings between two lines of `"""`, `Examples:` followed by a table,
comments that start with `#`, and blank lines. Anything else is a problem. The Gherkin keywords
`Rule:`, `Example:`, `Scenarios:` and `Scenario` followed by any words other than `Outline` (such
as `Scenario Template:`) are refused wherever they appear; any other line of the description, such
as `Note: refunds are rare`, is prose. A file is at most 262144 bytes and 200 scenarios; a
`Scenario:` line inside a doc string is text and does not count.

Escapes, so that every spec is written and read back unchanged:

- Table cells: a backslash is written `\\` and a pipe `\|`. Reading a row takes `\|` as a pipe,
  `\\` as a backslash, any other backslash as itself and every other pipe as the end of a cell.
- Doc strings: a line whose text, without its indentation, is made only of backslashes and quotes
  and turns into `"""` when the escape is undone zero or more times is written escaped once more:
  each backslash as `\\` and each quote as `\"`. So `"""` is written `\"\"\"`, and `\"\"\"` is
  written `\\\"\\\"\\\"`. Undoing the escape reads `\\` as `\` and `\"` as `"`, and applies only
  to a line in which every character is part of such a pair. Reading undoes it exactly once on
  such a line and keeps every other line as written.

Canonical form: feature tags one per line, `Feature:` at column 0, description indented 2, a blank
line between blocks, scenario tags on one line indented 2 in the order id, judged, evidence,
boundary, reserved; steps indented 4; tables and doc strings indented 6 with cells padded to the
width of their column; `Examples:` indented 4 after a blank line, its table indented 6.

### Markers

In the files a suite lists as `tests`, a comment `spec: <spec>/<scenario>` declares that the test
below verifies that scenario. The comment starts with `//`, `///` or `#` after optional spaces.
The test name is the function declared in the eight lines after the marker: `fn <name>`,
`func <name>`, `def <name>`, or the first quoted text of `test(` or `it(`.

### `boundary.toml`

```toml
id = "billing"                         # the name of its folder
owns = ["crates/billing/**"]           # required
regenerates = ["crates/billing/src/**"]
inputs = ["Cargo.toml"]
depends_on = ["ledger"]
data = ["specs/ledger/**"]
fixtures = ["crates/billing/fixtures/**"]
surface = "specs/billing/surface.txt"

[[suite]]
id = "unit"
run = ["bazelisk", "test", "//crates/billing:test"]   # at least one argument
tests = ["crates/billing/tests/**/*.rs"]
results = "bazel-testlogs/**/test.xml"
timeout_minutes = 30                                  # default 30
```

A boundary delimits; it promises nothing. A boundary needs a scenario tagged
`@boundary:dependencies`; one tagged `@boundary:surface` when it names a surface; one tagged
`@boundary:data` when `data` is not empty.

Patterns: `*` is any run of characters inside one segment, `**` any number of whole segments
(at least one when it ends the pattern). Consecutive `**` segments count as one, and matching never
tries the same place of the pattern against the same place of the path twice, so its time grows
with the product of their lengths.

No pattern of `owns` or `inputs` may reach the ledger, whether or not a ledger file exists yet:
its segments are walked against those of `<specs dir>/ledger`; a `**` reaches it, a segment that
matches goes on, one that does not cannot reach it, a pattern that ends first cannot reach it, and
a pattern with segments left after the ledger folder reaches it. `data` may name the ledger.
Problems about a key name the line of that key, or of the pattern they are about.

### Decisions

```markdown
+++
id = "keep-ledger"            # the name of the file
title = "Keep a ledger"
status = "accepted"           # proposed | accepted | rejected | superseded | deprecated
date = "2026-11-25"
supersedes = []
superseded_by = ""
+++

## Context
## Decision
## Consequences
## Confirmation
```

A decision that supersedes another needs the other one to be `superseded` and to name it in
`superseded_by`. A `superseded` decision needs a `superseded_by` that names a decision listing it
in `supersedes`. Either mismatch is `supersession_mismatch`.

### Reference prose

```markdown
## How refunds work
<!-- section: billing.refunds-overview explains: billing.refunds, boundary:billing, decision:keep-ledger -->
```

The comment is on the line right after the heading. Any line there that starts with `<!--` is
read as the section comment, and one that does not read as above is a problem at its line. Text
before the first `##` belongs to no section.

### The ledger

```
specs/ledger/<component>/suites/<suite>.toml               inputs = "sha256:…"
specs/ledger/<component>/scenarios/<spec>/<scenario>.toml  [adequate] and [judged]
specs/ledger/reference/<section>.toml                      [reference]
```

```toml
[adequate]
inputs = "sha256:…"
verdict = "met"

[judged]
inputs = "sha256:…"
verdict = "met"
evidence = ["sha256:…"]
```

Tables are written in the order adequate, judged, with a blank line between them; `evidence` only
when there is some. No other key is accepted. A `[judged]` table of a scenario tagged
`@judged:capture` or `@judged:video` with no `evidence` is refused with `evidence_required`.

## Fingerprints

`sha256:` and 64 lowercase hexadecimal digits. Each is the SHA-256 of a text built from lines, so
that nothing but the inputs below can change it:

| Fingerprint | Inputs |
|---|---|
| Component | The path and the content hash of every listed file that matches `owns` or `inputs`, in path order, then the fingerprint of every component in `depends_on`, in id order |
| Suite | The component, the suite id, its arguments, each of its `tests` patterns, and the path and content hash of every listed file that matches one of them, in path order |
| Adequate | The canonical text of the scenario, the title, description and background of its spec, and the path and content hash of every test file that declares it |
| Judged | The canonical text of the scenario and, with `@evidence`, the path and content hash of every listed file that matches it; without it, the component |
| Reference | The text of the section and, for everything it explains, the canonical text of the spec or the content hash of the boundary or decision file |

## States

| State | Meaning |
|---|---|
| `valid` | Recorded with the current fingerprint and verdict `met` |
| `missing` | Never recorded |
| `stale` | Recorded with another fingerprint |
| `unmet` | Recorded with the current fingerprint and verdict `unmet` |
| `failed` | Test results were given and a test that declares the scenario did not pass: no case is that test, a case that is that test failed, had an error or was skipped, or the marker has no test name |
| `uncovered` | No test declares the scenario and it is not a judgment |

Items: `suite` for `<component>/<suite>`; `passes`, `adequate` and `judged` for a scenario;
`reference` for a section. A scenario that tests declare has `passes` and `adequate`; one that no
test declares and is not a judgment has only `passes`, as `uncovered`; a judgment has `judged`.
Items are ordered by subject, then by kind.

Report status: `unreadable` with problems, `clean` when every item is `valid`, `pending`
otherwise. Problems are ordered by path, line and code.

## The command

```
alondra spec validate [--json] [--junit <file>]      0 clean, 1 pending, 2 unreadable
alondra spec run <component> [--suite <id>]         0 recorded, 1 a suite failed, 2 not runnable
alondra spec record <subject> --kind adequate|judged|reference --verdict met|unmet [--evidence <file>]...
alondra spec list
alondra spec packet <component>
```

`validate` never runs a command, starts an agent or writes a file.

`run` executes each command of the plan without a shell, from the root of the repository, in a
process group of its own. At its time limit every process of that group is stopped, and so is it
when `alondra spec run` itself is interrupted. Its output is shown up to 8 MiB and dropped after
that. Only a suite that ends well within its limit gets its ledger entry.

`run` and `record` write a ledger entry to a new file that then takes the place of the old one,
and never through a link: when a folder on the way or the entry itself is a link, nothing is
written and the command ends with 2 and `path_outside_repository`. A listed file that leaves the
checkout through a link is not read.

The command asks git for the files it tracks and the new ones it does not ignore, which is what a
commit of the whole working tree would hold, and reads them from the working tree. A file git
ignores never changes a result.

## The packet

```
# Component <id>

Specs are the only source of truth. Where any other text disagrees with a spec, the spec wins.

## What you may write
## What you must not change
## Boundary
## Decisions
## Specs
## Reference (context, not a requirement)
```

Scenarios tagged `@reserved` are left out of the specs in a packet.

## Regeneration pilot

`make regeneration-pilot COMPONENT=spec-engine RUNS=5 MODEL=sonnet` gives a model, in a temporary
folder outside the checkout, the component without the files it regenerates and without the tests
of its reserved scenarios, together with its packet, and measures each result with the component's
real suites: suites that pass, tests that pass, reserved tests that pass, and, through the suite
that compares it, whether the public surface is the committed one. `BASELINE=1` hands the model a
short description and the surface instead of the packet and the tests. It costs tokens and builds,
and it replaces files of the checkout while it measures, so it is run by hand on a clean checkout
(`tools/regeneration_pilot.py`).
