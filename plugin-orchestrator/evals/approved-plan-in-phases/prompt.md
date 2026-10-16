---
name: approved-plan-in-phases
description: An approved plan in phases becomes one task per phase, each with its points and only its real dependencies
tags: [skill, phases]
runs: 3
max_turns: 20
allowed_tools: [Skill, mcp__plugin_alondra-orchestrator_alondra-orchestrator__title_request, mcp__plugin_alondra-orchestrator_alondra-orchestrator__create_task]
append_system_prompt: |
  You are the Alondra orchestrator handling one batch of its inbox. Request effort is auto. The
  project folder is /work/service. Act with the Alondra tools; do not answer in prose only.
---

Load the alondra-orchestrator skill, then handle this batch:

The user approved the plan that Task “Can the screens be fast?” (t-research) proposed in its final
result. Request implement-plan holds that plan: name it with title_request, then create its
implementation tasks.

Name each task t-phase-N, N being its phase number.

The approved plan:

# Fast screens

## Phase 1: command guards
Delivers: a pure guard per command, checked against running it. Measured by the guard tests.
Depends on: none
- [ ] A guard for every command
- [ ] Guards compared with running each command

## Phase 2: read index
Delivers: the read index, committed with each revision. Measured by the index-matches-state test.
Depends on: phase 1
- [ ] Read index stored next to the domain
- [ ] Index matches the state after every command

## Phase 3: screens on the index
Delivers: every screen reads the index. Measured by the 50 ms view budget.
Depends on: phase 2
- [ ] Every screen reads the index

## Phase 4: notifications on the index
Delivers: push notifications read the index. Measured by the notification tests.
Depends on: phase 2
- [ ] Notifications read the index

## Phase 5: apps on the new fields
Delivers: phone and desktop apps read the new API fields. Measured by app snapshots.
Depends on: phase 7
- [ ] Phone app reads the new fields
- [ ] Desktop app reads the new fields

## Phase 6: delete the old relay routes
Delivers: the old routes are gone. Measured by the relay tests.
Depends on: phase 5
- [ ] Delete the old relay routes

## Phase 7: public API on the index
Delivers: the API exposes the index. Measured by the API contract tests.
Depends on: phase 2
- [ ] API exposes the index
