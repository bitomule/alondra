# The spec engine

What the spec engine is for and how its pieces fit, in plain words. The specs under
`specs/spec-engine/` are the source of truth; where this page disagrees with one, the spec wins.

## Specs, and the tests that say which scenario they verify
<!-- section: spec-engine.specs-and-tests explains: spec-engine.feature-format, spec-engine.parts, spec-engine.scenario-ids, spec-engine.markers, decision:gherkin-syntax-without-a-runner, decision:own-gherkin-reader -->

A spec is one `.feature` file that describes one behaviour as a list of scenarios. Each scenario
has an identifier of its own, so you can reword its title as often as you like and nothing that
points at it breaks. The file uses the Given/When/Then syntax only as a way of writing: nothing
runs the steps.

A spec may keep diagrams and mockups next to its file, cited with a tag by the spec or by one
scenario. They illustrate the rules, and changing one asks for the validations of what it
illustrates again.

Tests stay where they are, in the framework each project already uses. A test says which scenario
it verifies with a comment above it. The spec never lists its tests, so the link is written once,
next to the code that would break it.

## Components and what they promise
<!-- section: spec-engine.components explains: spec-engine.boundary, spec-engine.contract, spec-engine.patterns, boundary:spec-engine, decision:component-boundaries -->

A component is a part of the project that can be deleted and written again. Its boundary file
says which files it owns, which of them may be regenerated, what else its result depends on and
how its tests run. The boundary only draws the line. Everything a new implementation must keep
(the public surface, what it may depend on, the stored data it must still read) is written as a
scenario with a test, like any other behaviour.

## Knowing what is validated
<!-- section: spec-engine.validation explains: spec-engine.fingerprints, spec-engine.ledger, spec-engine.validation, spec-engine.run-plan, decision:engine-reads-through-its-caller, decision:ledger-holds-fingerprints-only, decision:engine-never-runs-anything, decision:suite-level-pass -->

Every validation is stored with a fingerprint of what it looked at. When any of that changes, the
validation is stale and has to be done again; when nothing it looked at changes, it stays valid.
That is how a project keeps its specs alive without validating everything on every change.

There are two kinds of validation. Running a suite of tests is deterministic: it passed or it did
not. Deciding that a test really checks its scenario, or that a capture or a video shows what the
scenario says, is a judgment, and it is always labelled as one.

Validating only compares and reports. It never runs a command and never starts an agent: it says
what is missing, and whoever is in charge runs it and records the result.

## Settings, decisions and this reference
<!-- section: spec-engine.documents explains: spec-engine.settings, spec-engine.decisions, spec-engine.reference, spec-engine.packet, decision:settings-in-one-committed-file, decision:three-levels-only-specs-rule -->

A project turns all of this on in `alondra.toml`. Without that file nothing changes.

Lasting decisions are short records with a status. A spec can cite the decisions it rests on, and
only accepted ones. Pages like this one explain the product in plain language: each section says
which specs it explains, and it is flagged for review when they change. An agent that starts work
on a component receives a packet with its specs, its boundary and the decisions behind them, and
the packet always says that specs win over any other text.
