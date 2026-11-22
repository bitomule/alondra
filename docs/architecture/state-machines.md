# State machines

Every explicit and implicit state machine of Alondra: its states, source of truth, invariants,
allowed and rejected transitions, terminals, persistence across restarts and concurrency. It also
lists the invalid states and transitions the code can reach, through retries, duplicates,
cancellations and concurrency, and the plan that fixes them. This is the single canonical file for
it: whoever changes one of these machines updates its section here in the same pull request.

- **Audited at**: commit `1567967b9` of `main` (2026-11-22).
- **Scope**: domain (`crates/domain`, `crates/domain-rules`, `crates/core`), engine
  (`crates/engine-*`, `crates/daemon-*`, `crates/alondrad`), persistence (`crates/store-sqlite`
  and the JSON state files of the Alondra home), providers and agent hosts (`agent-host/`,
  `crates/provider-*`, `crates/agent-*`, `crates/codex-protocol`), the relay (`crates/relay`),
  clients (`crates/client-*`, `crates/cli*`, `crates/view-model`, `apple/`) and the machine
  model (`crates/machine-model`).
- **Out of scope**: `web/` (abandoned). Code that only formats or renders and holds no state.
- **Method**: reading the code only. No build, test or external tool was run. Every finding
  labelled CONFIRMED was traced end to end, each guard checked at `path:line`, with no open
  assumption. HYPOTHESIS says exactly what is missing to confirm it. Line numbers refer to the
  audited commit; later commits move them.
- **Limits**: the Apple apps were read, not run. Timing claims (how likely a race is) are
  estimates from the code paths, not measurements. Security-relevant findings, if any, are
  handled privately and are not part of this file.

Severity: **S1** data loss or corruption, or a task stuck with no way out; **S2** wrong visible
state, or duplicated or lost work that can be recovered; **S3** cosmetic or rare.

## Contents

1. [Findings by priority](#findings-by-priority)
2. [Fix groups](#fix-groups)
3. [Referred to the tasks that own them](#referred-to-the-tasks-that-own-them)
4. [Product decisions](#product-decisions)
5. Catalog and findings by area:
   [Task and node lifecycle](#task-and-node-lifecycle-life) ·
   [Cards, plans, documents and requests](#cards-plans-documents-and-requests-cards) ·
   [Engine](#engine-admission-host-ownership-machine-watch-and-transports-engine) ·
   [Persistence and restarts](#persistence-journals-deferred-effects-and-restarts-store) ·
   [Pull requests, worktrees, closing and retention](#pull-requests-merges-worktrees-closing-and-retention-gh) ·
   [Orchestrator and schedules](#orchestrator-and-schedules-orch) ·
   [Agent hosts and providers](#agent-hosts-and-providers-prov) ·
   [Devices, pairing, self-update and relay](#daemon-devices-pairing-self-update-and-relay-server) ·
   [Clients](#clients-terminal-transport-mcp-and-apple-apps-clients)

## Findings by priority

CONFIRMED findings that no other task owns, most severe first. The group column links each one
to its fix group and issue.

| Sev. | Finding | Probability | Group |
|---|---|---|---|
| S1 | [F-GH-2](#f-gh-2--an-already-answered-delete-unsaved-changes-card-deletes-the-recreated-worktree-of-a-reopened-task-on-every-closer-pass) · An already-answered "delete unsaved changes?" card deletes the recreated worktree of a reopened task, on every closer pass | low | [G1](#g1--never-delete-what-a-live-task-or-the-user-still-owns) · #518 |
| S1 | [F-GH-4](#f-gh-4--closing-a-task-with-a-merged-pr-deletes-the-branch-of-a-remote-less-repo-another-repo-with-commits-that-exist-only-there-pr-503) · Closing a task with a merged PR deletes the branch of a remote-less repo (another repo) with commits that exist only there (PR #503) | low | [G1](#g1--never-delete-what-a-live-task-or-the-user-still-owns) · #518 |
| S1 | [F-ORCH-5](#f-orch-5--a-non-domain-error-in-the-schedule-runner-stops-the-whole-daemon-with-an-unreadable-optionstoml-it-enters-a-restart-loop) · A non-domain error in the schedule runner stops the whole daemon; with an unreadable `options.toml` it enters a restart loop | low | [G3](#g3--the-daemon-survives-its-own-restarts-and-recoverable-errors) · #520 |
| S1 | [F-STORE-4](#f-store-4--a-released-task-whose-closed-task-nodesjson-entry-is-lost-is-left-without-a-node-forever-nothing-relaunches-it-or-says-so) · A released task whose `closed-task-nodes.json` entry is lost is left without a node forever: nothing relaunches it or says so | low | [G2](#g2--a-released-task-always-has-a-way-back) · #519 |
| S2 | [F-CARDS-1](#f-cards-1--a-request-whose-tasks-ended-depends-on-a-single-message-to-the-orchestrator-if-that-batch-does-not-close-it-it-stays-open-forever-with-no-result-card-issue-353) · A request whose tasks ended depends on a single message to the orchestrator: if that batch does not close it, it stays open forever with no result card (issue #353) | medium | [G5](#g5--every-request-and-task-has-someone-who-moves-it-next) · #522 |
| S2 | [F-CARDS-2](#f-cards-2--replacing-one-question-of-a-group-leaves-the-group-incomplete-forever-the-node-never-receives-the-other-answers) · Replacing one question of a group leaves the group incomplete forever: the node never receives the other answers | low | [G6](#g6--card-protocol-replacements-approvals-and-stale-deliveries) · #523 |
| S2 | [F-CARDS-3](#f-cards-3--requesting-changes-or-just-replying-on-an-already-approved-plan-while-its-task-closes-disapproves-it-but-leaves-the-implementation-request-alive-the-plan-is-implemented-twice-or-with-the-old-version) · Requesting changes (or just replying) on an already approved plan while its task closes disapproves it but leaves the implementation request alive: the plan is implemented twice or with the old version | low-medium | [G6](#g6--card-protocol-replacements-approvals-and-stale-deliveries) · #523 |
| S2 | [F-CARDS-4](#f-cards-4--a-new-revision-of-a-document-leaves-dead-the-final-delivery-that-carries-the-previous-one-neither-accepting-nor-requesting-changes-works-and-nobody-returns-it) · A new revision of a document leaves dead the final delivery that carries the previous one: neither accepting nor requesting changes works and nobody returns it | low | [G6](#g6--card-protocol-replacements-approvals-and-stale-deliveries) · #523 |
| S2 | [F-CARDS-5](#f-cards-5--replacing-the-current-final-delivery-with-a-card-that-is-not-another-final-delivery-withdraws-it-without-returning-it-the-task-stays-delivered-with-no-card-for-the-user) · Replacing the current final delivery with a card that is not another final delivery withdraws it without returning it: the task stays "Delivered" with no card for the user | low-medium | [G6](#g6--card-protocol-replacements-approvals-and-stale-deliveries) · #523 |
| S2 | [F-CLIENTS-A1](#f-clients-a1--a-notification-answer-that-did-not-reach-the-mac-only-goes-out-when-that-macs-feed-reconciles-with-several-macs-the-one-for-a-non-selected-mac-waits-until-the-user-switches-to-it) · A notification answer that did not reach the Mac only goes out when that Mac's feed reconciles; with several Macs, the one for a non-selected Mac waits until the user switches to it | low | [G10](#g10--apple-apps-optimistic-actions-and-per-mac-state) · #527 |
| S2 | [F-CLIENTS-A2](#f-clients-a2--in-tasks-and-in-the-task-detail-iphone-the-optimistic-action-has-no-notice-at-30-s-withdrawing-a-request-deleting-completed-or-cancellingpausing-a-task-stays-hidden-while-the-mac-does-not-confirm) · In Tasks and in the task detail (iPhone) the optimistic action has no notice at 30 s: withdrawing a request, deleting Completed or cancelling/pausing a task stays hidden while the Mac does not confirm | medium | [G10](#g10--apple-apps-optimistic-actions-and-per-mac-state) · #527 |
| S2 | [F-CLIENTS-A3](#f-clients-a3--qr-pairing-fails-halfway-if-the-access-token-expires-or-there-is-a-cut-while-waiting-for-the-macs-approval-the-mac-can-be-left-with-a-device-without-keys) · QR pairing fails halfway if the access token expires or there is a cut while waiting for the Mac's approval; the Mac can be left with a device without keys | medium | [G10](#g10--apple-apps-optimistic-actions-and-per-mac-state) · #527 |
| S2 | [F-CLIENTS-A4](#f-clients-a4--after-switching-mac-the-live-activity-keeps-showing-the-previous-macs-frozen-numbers-as-just-measured-and-covers-the-sealed-reports-the-relay-brings-from-it) · After switching Mac, the Live Activity keeps showing the previous Mac's frozen numbers as "just measured" and covers the sealed reports the relay brings from it | medium | [G10](#g10--apple-apps-optimistic-actions-and-per-mac-state) · #527 |
| S2 | [F-CLIENTS-R1](#f-clients-r1--the-tui-automatic-restart-throws-away-answers-not-yet-confirmed) · The TUI automatic restart throws away answers not yet confirmed | medium | [G9](#g9--terminal-client-and-local-pairing-recover-by-themselves) · #526 |
| S2 | [F-CLIENTS-R4](#f-clients-r4--a-terminal-whose-local-device-was-removed-stays-locked-out-forever) · A terminal whose local device was removed stays locked out forever | low | [G9](#g9--terminal-client-and-local-pairing-recover-by-themselves) · #526 |
| S2 | [F-ENGINE-1](#f-engine-1--the-slot-of-an-admitted-start-expires-after-3-min-even-if-the-node-does-not-exist-yet-the-gate-admits-above-the-limit) · The slot of an admitted start expires after 3 min even if the node does not exist yet: the gate admits above the limit | low | [G4](#g4--the-admission-gate-counts-what-is-really-running) · #521 |
| S2 | [F-ENGINE-12](#f-engine-12--a-retake-in-progress-and-the-adoption-of-a-model-transition-take-the-same-host-at-once-a-false-host-lost-card-or-two-live-owners-for-one-agent-formerly-h-engine-1) · A retake in progress and the adoption of a model transition take the same host at once: a false "host lost" card, or two live `Owner`s for one agent | low | [G8](#g8--one-owner-per-host-and-provider-facts-in-order) · #525 |
| S2 | [F-ENGINE-3](#f-engine-3--the-pause-of-a-queued-task-lives-in-admissionjson-apart-from-launch_queue-it-can-be-lost-duplicated-or-stay-forever) · The pause of a queued task lives in `admission.json`, apart from `launch_queue`: it can be lost, duplicated or stay forever | — | [G4](#g4--the-admission-gate-counts-what-is-really-running) · #521 |
| S2 | [F-ENGINE-6](#f-engine-6--the-watch-advances-the-disk-incident-on-every-look-not-on-every-sample-with-watched-volumes-any-domain-change-restarts-the-recovery-and-without-them-one-sample-counts-twice) · The watch advances the disk incident on every look, not on every sample: with watched volumes any domain change restarts the recovery (and without them one sample counts twice) | medium | [G4](#g4--the-admission-gate-counts-what-is-really-running) · #521 |
| S2 | [F-ENGINE-7](#f-engine-7--the-stretches-of-build-volumes-do-not-survive-a-restart-working-nodes-then-reserve-at-most-1-gb-there) · The stretches of build volumes do not survive a restart: working nodes then reserve at most 1 GB there | medium | [G4](#g4--the-admission-gate-counts-what-is-really-running) · #521 |
| S2 | [F-ENGINE-9](#f-engine-9--a-volume-that-is-not-a-build-volume-can-stay-learned-forever-and-hold-every-start-and-the-turns) · A volume that is not a build volume can stay learned forever and hold every start and the turns | low | [G4](#g4--the-admission-gate-counts-what-is-really-running) · #521 |
| S2 | [F-GH-1](#f-gh-1--the-watcher-closes-on-github-the-pr-of-a-task-the-user-just-reopened-or-that-another-task-just-took) · The watcher closes on GitHub the PR of a task the user just reopened (or that another task just took) | low-medium | [G7](#g7--github-no-closing-merging-or-reading-against-a-stale-picture) · #524 |
| S2 | [F-GH-3](#f-gh-3--the-startup-sweep-of-cancelled-tasks-deletes-a-worktree-that-task-handed-to-another-if-the-other-is-already-closing-or-the-user-kept-it) · The startup sweep of cancelled tasks deletes a worktree that task handed to another, if the other is already closing or the user kept it | low | [G1](#g1--never-delete-what-a-live-task-or-the-user-still-owns) · #518 |
| S2 | [F-GH-5](#f-gh-5--post-merge-work-declared-and-the-node-idle-the-task-stays-working-forever-its-host-is-never-released-and-it-holds-a-slot) · Post-merge work declared and the node idle: the task stays "Working" forever, its host is never released and it holds a slot | medium-low | [G5](#g5--every-request-and-task-has-someone-who-moves-it-next) · #522 |
| S2 | [F-GH-7](#f-gh-7--a-merge-that-github-blocks-for-a-required-review-goes-back-to-the-node-and-invalidates-the-users-acceptance-in-a-loop) · A merge that GitHub blocks for a required review goes back to the node and invalidates the user's acceptance, in a loop | medium | [G7](#g7--github-no-closing-merging-or-reading-against-a-stale-picture) · #524 |
| S2 | [F-GH-8](#f-gh-8--the-github-api-limit-still-raises-technical-cards-one-per-pr-in-the-watcher-and-another-per-pr-in-the-merger) · The GitHub API limit still raises technical cards: one per PR in the watcher and another per PR in the merger | low | [G7](#g7--github-no-closing-merging-or-reading-against-a-stale-picture) · #524 |
| S2 | [F-LIFE-1](#f-life-1--an-unstarted-task-that-waits-for-other-tasks-shows-as-queued-slot-wait-not-waiting-for-tasks) · An unstarted task that waits for other tasks shows as "Queued" (slot wait), not "Waiting for tasks" | high | [G5](#g5--every-request-and-task-has-someone-who-moves-it-next) · #522 |
| S2 | [F-LIFE-2](#f-life-2--reopening-a-task-whose-sdk-node-the-user-left-open-creates-a-claude---bg-execution-nobody-runs) · Reopening a task whose SDK node the user left open creates a `claude --bg` execution nobody runs | low | [G2](#g2--a-released-task-always-has-a-way-back) · #519 |
| S2 | [F-ORCH-1](#f-orch-1--a-daemon-restart-in-the-middle-of-a-batch-stops-the-orchestrator-until-the-user-answers-a-card) · A daemon restart in the middle of a batch stops the orchestrator until the user answers a card | medium | [G3](#g3--the-daemon-survives-its-own-restarts-and-recoverable-errors) · #520 |
| S2 | [F-ORCH-2](#f-orch-2--replanning-reminders-feed-themselves-one-batch-per-reminder-without-end) · Replanning reminders feed themselves: one batch per reminder, without end | low-medium | [G5](#g5--every-request-and-task-has-someone-who-moves-it-next) · #522 |
| S2 | [F-ORCH-3](#f-orch-3--after-24-h-of-launch-failures-requests-are-left-untreated-and-the-watchdog-restarts-in-vain-every-5-min) · After 24 h of launch failures, requests are left untreated and the watchdog restarts in vain every 5 min | low | [G3](#g3--the-daemon-survives-its-own-restarts-and-recoverable-errors) · #520 |
| S2 | [F-ORCH-4](#f-orch-4--a-scheduled-launch-that-did-start-is-considered-failed-after-a-restart-and-answering-the-card-cancels-the-live-task) · A scheduled launch that did start is considered failed after a restart, and answering the card cancels the live task | low | [G3](#g3--the-daemon-survives-its-own-restarts-and-recoverable-errors) · #520 |
| S2 | [F-PROV-1](#f-prov-1--a-usage-reading-measured-before-a-limit-erases-it-when-it-arrives-later-the-account-reads-as-having-margin-again) · A usage reading measured before a limit erases it when it arrives later: the account reads as having margin again | low | [G8](#g8--one-owner-per-host-and-provider-facts-in-order) · #525 |
| S2 | [F-SERVER-D1](#f-server-d1--a-transient-failure-on-the-first-enrollment-permanently-revokes-a-devices-relay-access-while-it-still-shows-as-paired) · A transient failure on the first enrollment permanently revokes a device's relay access while it still shows as paired | medium-low | [G11](#g11--devices-relay-and-self-update) · #528 |
| S2 | [F-SERVER-D4](#f-server-d4--if-the-macs-own-terminal-is-revoked-the-tui-stays-on-device-not-paired-forever) · If the Mac's own terminal is revoked, the TUI stays on «device not paired» forever | low | [G9](#g9--terminal-client-and-local-pairing-recover-by-themselves) · #526 |
| S2 | [F-SERVER-R4](#f-server-r4--if-live-activity-pruning-fails-after-a-revocation-the-removed-mac-keeps-the-live-activity-alive-indefinitely) · If Live Activity pruning fails after a revocation, the removed Mac keeps the Live Activity alive indefinitely | low | [G11](#g11--devices-relay-and-self-update) · #528 |
| S2 | [F-STORE-1](#f-store-1--a-second-process-applies-restart-recovery-to-the-live-database) · A second process applies restart recovery to the live database | low | [G3](#g3--the-daemon-survives-its-own-restarts-and-recoverable-errors) · #520 |
| S2 | [F-STORE-2](#f-store-2--alondrad---preflight-does-not-exercise-the-json-files-the-real-startup-requires) · `alondrad --preflight` does not exercise the JSON files the real startup requires | low | [G3](#g3--the-daemon-survives-its-own-restarts-and-recoverable-errors) · #520 |
| S3 | [F-CARDS-6](#f-cards-6--createtask-and-setdependencies-accept-an-already-cancelled-dependency-the-task-is-born-waiting-for-something-that-will-not-come-issue-323) · `CreateTask` and `SetDependencies` accept an already cancelled dependency: the task is born waiting for something that will not come (issue #323) | medium | [G5](#g5--every-request-and-task-has-someone-who-moves-it-next) · #522 |
| S3 | [F-CLIENTS-A5](#f-clients-a5--if-preparing-an-answer-fails-the-queue-iphone-and-the-sidebar-mac-leave-the-card-sending-forever) · If preparing an answer fails, the queue (iPhone) and the sidebar (Mac) leave the card "sending" forever | low | [G10](#g10--apple-apps-optimistic-actions-and-per-mac-state) · #527 |
| S3 | [F-CLIENTS-R2](#f-clients-r2--direct-http-route-a-refusal-from-the-mac-is-read-as-cut-midway) · Direct HTTP route: a refusal from the Mac is read as "cut midway" | low | [G9](#g9--terminal-client-and-local-pairing-recover-by-themselves) · #526 |
| S3 | [F-CLIENTS-R3](#f-clients-r3--the-tui-accepts-an-old-home-that-arrives-late-and-overwrites-the-new-one) · The TUI accepts an old home that arrives late and overwrites the new one | low | [G9](#g9--terminal-client-and-local-pairing-recover-by-themselves) · #526 |
| S3 | [F-ENGINE-10](#f-engine-10--disk_room_for-decides-the-next-one-goes-alone-with-a-disk_working-that-pace-can-empty-mid-call) · `disk_room_for` decides "the next one goes alone" with a `disk_working` that `pace` can empty mid-call | very | [G4](#g4--the-admission-gate-counts-what-is-really-running) · #521 |
| S3 | [F-ENGINE-11](#f-engine-11--two-states-of-the-hook-transport-are-unreachable-and-their-documentation-describes-behaviour-that-does-not-exist) · Two states of the hook transport are unreachable and their documentation describes behaviour that does not exist | always | [G12](#g12--the-architecture-docs-match-the-code) · #529 |
| S3 | [F-ENGINE-2](#f-engine-2--auto_launch-admissions-that-never-launch-hold-a-slot-for-up-to-3-min) · `auto_launch` admissions that never launch hold a slot for up to 3 min | low | [G4](#g4--the-admission-gate-counts-what-is-really-running) · #521 |
| S3 | [F-ENGINE-4](#f-engine-4--a-turn-with-a-granted-slot-loses-its-place-in-the-queue-after-a-restart) · A turn with a granted slot loses its place in the queue after a restart | medium | [G4](#g4--the-admission-gate-counts-what-is-really-running) · #521 |
| S3 | [F-ENGINE-5](#f-engine-5--task-statesmd-describes-a-timer-that-no-longer-exists-and-omits-the-ones-that-do) · `task-states.md` describes a timer that no longer exists and omits the ones that do | not | [G12](#g12--the-architecture-docs-match-the-code) · #529 |
| S3 | [F-ENGINE-8](#f-engine-8--own_worktree-does-not-delete-the-worktree-of-a-task-that-ended-while-starting-the-domain-command-rejects-it-before-reaching-that-branch) · `own_worktree` does not delete the worktree of a task that ended while starting: the domain command rejects it before reaching that branch | low | [G1](#g1--never-delete-what-a-live-task-or-the-user-still-owns) · #518 |
| S3 | [F-GH-6](#f-gh-6--an-attachment-of-a-user-message-to-an-open-task-is-deleted-after-30-days-even-though-the-task-is-still-open) · An attachment of a user message to an open task is deleted after 30 days even though the task is still open | low | [G1](#g1--never-delete-what-a-live-task-or-the-user-still-owns) · #518 |
| S3 | [F-GH-9](#f-gh-9--the-merger-rereads-github-on-every-domain-commit-for-a-pr-closed-outside-or-with-a-new-head-until-the-watcher-sees-it) · The merger rereads GitHub on every domain commit for a PR closed outside or with a new head, until the watcher sees it | medium | [G7](#g7--github-no-closing-merging-or-reading-against-a-stale-picture) · #524 |
| S3 | [F-ORCH-6](#f-orch-6--the-orchestrators-progress-marks-have-done-nothing-since-277) · The orchestrator's progress marks have done nothing since #277 | low | [G3](#g3--the-daemon-survives-its-own-restarts-and-recoverable-errors) · #520 |
| S3 | [F-ORCH-7](#f-orch-7--an-accepted-cancellation-http-200-can-be-lost-orchestratorjson-is-written-without-a-lock-from-two-threads) · An accepted cancellation (HTTP 200) can be lost: `orchestrator.json` is written without a lock from two threads | very | [G3](#g3--the-daemon-survives-its-own-restarts-and-recoverable-errors) · #520 |
| S3 | [F-ORCH-8](#f-orch-8--create_task-carries-no-priority-and-automatic-launch-wakes-in-the-same-commit-issue-352) · `create_task` carries no priority and automatic launch wakes in the same commit (issue #352) | medium | [G5](#g5--every-request-and-task-has-someone-who-moves-it-next) · #522 |
| S3 | [F-SERVER-D5](#f-server-d5--a-direct-request-from-a-revoked-or-conflicting-id-reaches-the-user-approval-fails-with-500-and-the-client-ends-up-seeing-expired) · A direct request from a revoked or conflicting id reaches the user, approval fails with 500 and the client ends up seeing «Expired» | low | [G11](#g11--devices-relay-and-self-update) · #528 |
| S3 | [F-SERVER-D7](#f-server-d7--a-failed-brew-upgrade-blocks-self-update-of-that-version-until-another-one-is-released) · A failed `brew upgrade` blocks self-update of that version until another one is released | medium | [G11](#g11--devices-relay-and-self-update) · #528 |
| S3 | [F-SERVER-R6](#f-server-r6--a-crash-between-take-and-the-send-leaves-live--someno-token-and-blocks-the-start-for-up-to-7-h-45) · A crash between `take` and the send leaves `live = Some(no token)` and blocks the start for up to 7 h 45 | low | [G11](#g11--devices-relay-and-self-update) · #528 |
| S3 | [F-STORE-3](#f-store-3--retry-of-an-uncertain-orchestrator-message-older-than-24-h-discards-itself) · "Retry" of an uncertain orchestrator message older than 24 h discards itself | medium | [G3](#g3--the-daemon-survives-its-own-restarts-and-recoverable-errors) · #520 |

Hypotheses (not counted above) are listed at the end of each area, with what is missing to
confirm them. The fix work does not treat a hypothesis as fixed: it either confirms it with a
failing test first or leaves it listed.

## Fix groups

Related causes are grouped so that one change fixes them together. Each group is one GitHub
issue. The groups are independent unless *Depends on* says otherwise; the order below is the
order of priority.

Every test named here follows the repository's rules: the lowest layer that shows the bug, a
virtual clock and barriers for timing and concurrency, no sleeps, and no external tools (git,
gh, claude, codex, node, launchctl, real hosts or scripts as doubles): the port doubles
(`engine-doubles`, `Worktrees`, `Disk`, `GitHub`, …) stand in for them. Each fix is proved by
ablation: revert the fix, see the new test fail, restore it, see it pass. The per-finding
sections in the catalog give the exact test, the expected failure today and what to revert.

### G1 · Never delete what a live task or the user still owns

Issue: #518 · Severity: S1 · Findings: F-GH-2, F-GH-4, F-GH-3, F-GH-6, F-ENGINE-8 and the
`files/` retention decision · Depends on: none.

- **Expected behaviour**: an answered «discard the unsaved changes?» card is applied once, only while
  its task is still cancelled and the path's record is still `KeptByUser`, and leaves a record that
  it was applied; `in_use` counts the task itself when it has not ended. A branch is deleted only
  when the pull request whose head is that branch, in that repository, merged, or when the branch
  holds no commit that a remote or the default branch lacks; never in a repository without a remote
  on the strength of another repository's merged PR. The startup sweep of cancelled tasks reads how
  each launch folder's record ended and never touches one handed over or kept by the user. A worktree
  of a task that ended while it was starting is removed then, not at the next start. Retention keeps
  a message's attachments while its task is open, and deletes only inside Alondra's known folders of
  `files/`; anything else is listed, never deleted (user decision).
- **Files**: `crates/engine-worktrees/src/cancelled_worktrees.rs`, `task_worktrees.rs`,
  `crates/system/src/worktree.rs` (the branch decision as a pure function of facts),
  `crates/engine-doubles/src/worktrees.rs` and its contract, `crates/daemon-support/src/files_retention.rs`,
  the store retention of attachments, `docs/on-your-mac.md`.
- **Tests**: engine tests of the closer and of `cancelled_worktrees` with the `Worktrees`/`Disk`
  doubles (cancel with a dirty worktree, answer «delete», reopen, the double recreates the folder with
  an uncommitted file, run another closer pass: the folder must stay); a pure test of the branch
  decision and a `Worktrees` contract case for a repository without a remote; a retention test with
  an unknown top-level entry older than 30 days and an attachment of an open task.

### G2 · A released task always has a way back

Issue: #519 · Severity: S1 · Findings: F-STORE-4, F-LIFE-2, H-LIFE-4 · Depends on: none ·
Related: issue #323 (new evidence).

- **Expected behaviour**: whether a released task relaunches by itself or asks the user is a domain
  fact written in the same transaction as `ReleaseTask`/`RecordNodeEnd`, not a JSON file written
  afterwards. The domain's own `Ownerless` detection (`holder()`, today only called from tests) runs
  in production and turns any open task nobody holds into a relaunch or a card. Reopening a task whose
  SDK node the user kept open reopens it without a node, and the gate starts a new SDK execution that
  resumes the saved session (default decision); the domain never records a `--bg` execution on an SDK
  node.
- **Files**: `crates/daemon-engine/src/node_exit.rs`, `live.rs`, `crates/engine-sdk/src/task_closer.rs`,
  `auto_launch.rs`, `start_retries.rs`, `crates/daemon-engine/src/start_failures.rs`,
  `crates/domain-rules/src/ownership.rs`, `rules.rs` (release and reopen), `crates/views/src/tasks.rs`.
- **Tests**: a domain test that releases a task and checks the relaunch fact is in the same commit; an
  engine test that releases a task, drops the in-memory/JSON entry (simulated crash between the two
  writes) and checks the task is relaunched or asked about; a domain test for reopen with a kept SDK
  node. Ablation: write the fact after the commit again and see the test fail.

### G3 · The daemon survives its own restarts and recoverable errors

Issue: #520 · Severity: S1 (F-ORCH-5), S2 otherwise · Findings: F-ORCH-5, F-ORCH-1, F-ORCH-3,
F-ORCH-4, F-ORCH-6, F-ORCH-7, F-STORE-1, F-STORE-2, F-STORE-3 · Depends on: none.

- **Expected behaviour**: a failure of one scheduled run (unreadable options, a launch error, a
  non-ordinary claim error) is recorded on that run and the schedule worker goes on; only storage
  errors stop the daemon, if that policy stays. A batch of the orchestrator cut by a restart resumes
  without the user (default decision). Boot failures of the orchestrator never leave requests
  untreated for 24 h. A scheduled launch that did start is recognised as started after a restart, and
  answering its card never cancels the live task. `orchestrator.json` has one writer. A second process
  never applies restart recovery to the live database. `--preflight` reads every JSON file startup
  requires. A manual retry of an expired orchestrator message is refused with a clear message. The
  orchestrator's progress marks either drive its watchdog again or are removed.
- **Files**: `crates/engine-sdk/src/domain_schedule_runner.rs`, `crates/alondrad/src/lib.rs`,
  `crates/engine-sdk/src/domain_orchestrator.rs` and the orchestrator inbox steps, the store's
  startup recovery, `crates/daemon-support/src/auth.rs` (preflight reads), the delivery CLI.
- **Tests**: `run_cycle` with a pending run and an invalid `options.toml` returns `Ok` and leaves the
  run pending with its reason (today `Err`; ablation: restore the `?` at `domain_schedule_runner.rs:415`);
  the orchestrator restart test through the path real startup takes (today the test only exercises
  `recover()`); a store test opening a second recovery against a live database.

### G4 · The admission gate counts what is really running

Issue: #521 · Severity: S2 · Findings: F-ENGINE-1, F-ENGINE-2, F-ENGINE-3, F-ENGINE-4,
F-ENGINE-6, F-ENGINE-7, F-ENGINE-9, F-ENGINE-10 · Depends on: none · Related: issue #352 (F-ENGINE-2).

- **Expected behaviour**: a slot is held from admission until the node exists or the launch fails,
  not for a fixed 3 minutes; an `auto_launch` admission that does not launch gives its slot back at
  once. The pause of a queued task is a domain fact (default decision). A turn granted a slot keeps
  its place across a restart. The disk incident advances per sample, not per look. Build-volume spans
  survive a restart. A volume that is not a build volume is never learned as one. `disk_room_for`
  reads one consistent snapshot.
- **Files**: `crates/engine-ports/src/admission.rs` and the gate, `crates/engine-sdk/src/auto_launch.rs`,
  `crates/daemon-engine/src/machine_watch.rs`, `disk_peaks.rs`, the launch queue in the store.
- **Tests**: gate tests with a virtual clock (admit, advance past 3 minutes without a node, ask again:
  today it admits above the limit); machine-watch tests feeding samples and looks separately; a
  restart test of the queue position.

### G5 · Every request and task has someone who moves it next

Issue: #522 · Severity: S2 · Findings: F-CARDS-1, F-CARDS-6, F-ORCH-2, F-ORCH-8, F-LIFE-1,
F-GH-5 · Depends on: none · Related: issues #353, #323, #350, #352.

- **Expected behaviour**: a request whose tasks all ended is reminded to the orchestrator at most
  twice, then Alondra closes it with its own «Done» card (user decision). `CreateTask` and
  `SetDependencies` refuse a dependency that is already cancelled, like `SetRelations` does. Replan
  reminders do not feed themselves: only rests caused by other work remind, and after K reminders the
  task shows a factual state (default decision). `create_task` carries its priority before
  auto-launch can wake. A task that waits for other tasks shows «waiting for tasks», never «queued»,
  because the aggregate records the dependency wait (default decision). After a merge, a turn that
  ends idle without declaring post-merge work again ends that work (documented rule).
- **Files**: `crates/domain-rules/src/rules.rs` (request closing, dependencies, post-merge),
  `crates/domain-rules/src/situation.rs`, `crates/domain/src/lifecycle.rs`, the orchestrator inbox
  (`orchestrator_inbox` steps/settle), `crates/engine-sdk/src/auto_launch.rs`.
- **Tests**: domain tests for each rule (a request with all tasks ended and two unanswered reminders
  ends Done; a dependency on a cancelled task is refused; a dependent task's `Situation` is
  «waiting for tasks»; post-merge declared, merge, idle turn end: the work ends); an orchestrator inbox
  test counting batches for an undecided replan.

### G6 · Card protocol: replacements, approvals and stale deliveries

Issue: #523 · Severity: S2 · Findings: F-CARDS-2, F-CARDS-3, F-CARDS-4, F-CARDS-5 · Depends
on: none · Related: issue #317.

- **Expected behaviour**: replacing one question of a group is refused (default decision). An approved
  plan is read-only: «request changes» creates a new request and «reply» reaches the orchestrator as a
  note (user decision). A new revision of a document hands the final delivery that carried the old one
  back to its node (as the PR sweep already does for pull requests), also for a node of another task
  sharing the document. Replacing the current final delivery with a card that is not a final delivery
  has the effects of `withdraw` (default decision), and `presentation()` refuses a withdrawn delivery.
- **Files**: `crates/domain-rules/src/rules.rs` (interactions, `replaces`, plan reviews, document
  revisions), `card_contract.rs`, `decisions.rs`.
- **Tests**: domain tests, one per rule, each with the exact sequence of its finding.

### G7 · GitHub: no closing, merging or reading against a stale picture

Issue: #524 · Severity: S2 · Findings: F-GH-1, F-GH-7, F-GH-8, F-GH-9 · Depends on: none.

- **Expected behaviour**: the PR watcher re-reads the domain before closing and never closes a PR that
  has a live owner again; the PR of a cancelled task is closed once and, if the user reopens it, handed
  to the user as an open PR (user decision). A merge blocked by a required review waits for that review
  instead of handing the task back and voiding the user's acceptance. GitHub's API limit never raises a
  card (watcher and merger). The merger does not re-read GitHub on every domain commit for a PR closed
  outside Alondra or with a new head.
- **Files**: `crates/engine-github/src/domain_github.rs`, `domain_merges.rs`, `open_pr_sweep.rs`,
  `github_world.rs`.
- **Tests**: engine tests with the `GitHub` double: reopen the task between the watcher's snapshot and
  its close; a merge answered «review required»; rate-limit answers on the watcher and merger paths.

### G8 · One owner per host, and provider facts in order

Issue: #525 · Severity: S2 · Findings: F-ENGINE-12, F-PROV-1 · Hypothesis to confirm or
refute with a test: H-ENGINE-8 · Depends on: coordination with the Codex recovery work (phase 2) (`take_host`).

- **Expected behaviour**: the adoption after a model transition waits its turn and never takes a host
  a resume is taking; a stale take never reaches `lost()` or ends the other owner's host. A usage
  reading measured before a limit rejection never erases it: readings compare measurement time, and
  the probe fence covers `account_usage`.
- **Files**: `crates/engine-sdk/src/sdk_owner.rs` (`take_host`), the model-transition adoption,
  `crates/core/src/usage.rs`, `crates/daemon-engine/src/usage_probe.rs`, the `RecordAccountUsage` rule.
- **Tests**: an engine test with barriers that interleaves a resume and an adoption of the same host;
  a usage test applying a reading measured before a rejection after it.

### G9 · Terminal client and local pairing recover by themselves

Issue: #526 · Severity: S2 · Findings: F-CLIENTS-R1, F-CLIENTS-R2, F-CLIENTS-R3,
F-CLIENTS-R4, F-SERVER-D4 · Depends on: none.

- **Expected behaviour**: the TUI's automatic restart keeps unconfirmed replies and resends them; a
  manual quit with replies still sending asks first (default decision). On the direct HTTP path a
  refusal from the Mac is reported as a refusal, not as «cut halfway». A late home never replaces a
  newer one. A terminal whose local device was removed pairs again once and says so; the Mac's own
  terminal cannot be revoked (default decisions).
- **Files**: `crates/cli-tui` (optimistic replies, restart), `crates/client-transport/src/direct_client.rs`,
  `local.rs`, `crates/cli` (device commands), `crates/daemon-support/src/auth.rs`.
- **Tests**: TUI state tests with transport doubles; a direct-client test with a refused response; a
  home ordering test.

### G10 · Apple apps: optimistic actions and per-Mac state

Issue: #527 · Severity: S2 · Findings: F-CLIENTS-A1, F-CLIENTS-A2, F-CLIENTS-A3,
F-CLIENTS-A4, F-CLIENTS-A5 · Depends on: none · Related: issue #386.

- **Expected behaviour**: a reply given from a notification that did not reach its Mac is delivered in
  the background through that Mac's connection as soon as it answers, selected or not (default
  decision). Tasks and task detail on iPhone get the 30-second notice the queue and the Mac have. QR
  pairing refreshes its access token while waiting for approval and retries transient failures like
  direct pairing. Switching Macs marks the previous Mac as not connected in the Live Activity, so its
  frozen numbers never cover newer sealed reports. A failed reply preparation emits `.settled`.
- **Files**: `apple/Data` (Sync, Transport, Pairing, LiveActivity, Notifications), `apple/Features/Sources`
  (ActionFeature, Tasks).
- **Tests**: Swift tests with transport and clock doubles, one per finding.

### G11 · Devices, relay and self-update

Issue: #528 · Severity: S2/S3 · Findings: F-SERVER-D1, F-SERVER-D5, F-SERVER-D7,
F-SERVER-R4, F-SERVER-R6 · Hypothesis: F-SERVER-R5 · Depends on: none.

- **Expected behaviour**: a transient failure on the first relay enrolment is retried or compensated,
  and anything left unresolved shows «pair again» (default decision). A direct request from an id that
  cannot be approved never reaches the user as an approvable card. A failed `brew upgrade` is retried
  3 times with a growing wait, then waits for the next version (default decision). A failed Live
  Activity prune after a revocation is retried. A crash between `take` and send never blocks the next
  start for hours.
- **Files**: `crates/daemon-support` (devices, pairing, self-update), `crates/relay` (activity,
  devices).
- **Tests**: relay and daemon-support tests with injected failures at each step.

### G12 · The architecture docs match the code

Issue: #529 · Severity: S3 · Findings: F-ENGINE-5, F-ENGINE-11, the differences listed at the
end of the lifecycle area, and the gaps of `crates/machine-model` · Depends on: G2, G4 and G5 (it
documents their final behaviour).

- **Expected behaviour**: `docs/architecture/task-states.md` describes the timers that exist (and
  only those), places `Situation` where it lives, includes `Replan` in the obligation table, and stops
  claiming «no known gaps» for the model test; the hook transport's docs describe only reachable
  states; the model test covers interruptions, reopening and Codex, or lists them as gaps.
- **Files**: `docs/architecture/task-states.md`, the hook transport docs, `crates/machine-model`.

## Referred to the tasks that own them

These findings fall inside another task's scope. The evidence is handed to that task; no
parallel fix is planned here.

| Finding | Owner | New evidence for the owner? |
|---|---|---|
| F-PROV-2 · reconciling an order the host never had, past its deadline, destroys the socket | the Codex recovery work (phase 2), point 8 | Yes: the exact path and the false «host lost» card |
| F-PROV-4 · a continued host refuses `start` without a typed code | the Codex recovery work (phase 2), point 7 | No |
| F-PROV-3 · Claude's checkpoint carries a dead host's open decisions and calls; at 64 a new decision is dropped | issue #328 and the stalled-node recovery work (phase 3) | Yes: the sets `forget_ended_host` keeps and the silent drop at the cap |
| H-LIFE-1 · a repeated `TurnStarted` after its `TurnEnded` reopens the turn | the stalled-node recovery work | Yes: the sequence and the guard that swallows the end |
| F-ENGINE-5 (part) · `opening_deadline` decides a host replacement outside the obligation policy | the stalled-node recovery work | Yes |
| H-PROV-2 · an input "offered" to Codex is only queued in the host | the provider adapters work (phase 3), point 10 | No |
| H-GH-1, H-GH-2 · identity of moved pull requests | the stable pull request identity work | See the hypotheses in the GH area |
| F-STORE-2 (part) · `--preflight` does not exercise the JSON state files startup needs | the tests-by-layer work (host model), point 30 (binary smoke test) | Yes: the list of files startup refuses on |

F-ENGINE-12 (two owners take the same host) is fixed here, in group G8, but it changes
`take_host`, the same function the Codex recovery work (phase 2) changes: the two must be coordinated.

## Product decisions

Decided by the user on 2026-11-22, on cards of this audit:

| Question | Decision |
|---|---|
| May Alondra delete, after 30 days, files of its `files/` folder that it did not create? (PR #503 deletes every top-level entry outside its known folders) | **Delete only inside its known folders** (tasks, results, uploads, images, documents). Anything else is listed in the retention report and never touched. |
| What do «request changes» or «reply» do on a plan already approved? (F-CARDS-3) | **An approved plan is read-only.** «Request changes» becomes a new request; «reply» reaches the orchestrator as a note on the build in progress, without stopping it. |
| If the user reopens on GitHub the pull request of a cancelled task, does Alondra close it again? (F-GH-1, open PR sweep) | **Close it once, then hand it to the user.** Alondra closes the PR once when the task is cancelled; if the user reopens it, Alondra respects that and shows it as an open PR for the user to decide. |
| Who closes a request whose tasks all ended, if the orchestrator does not? (F-CARDS-1, issue #353) | **Remind the orchestrator up to twice, then Alondra closes it** with its own «Done» card, without the orchestrator's summary. |

Defaults applied from rules the user has already set (optimistic UI with a notice after 30 s; no
technical cards for the user; a task only waits for capacity or the user; every request ends
with an outcome card; several Macs per client). Each can be changed in the review of this file:

| Finding | Default |
|---|---|
| F-LIFE-1 · where the wait for other tasks lives | In the lifecycle aggregate, as the single source (dependency records, `Progress::Dependency` reachable again). |
| F-LIFE-2 · reopening a task whose SDK node the user kept open | Reopen without a node; the gate starts a new SDK execution that resumes the saved session. |
| F-CARDS-2 · replacing one question of a group | Refused: the node replaces the whole group with another group. |
| F-CARDS-5 · replacing the current final delivery with a card that is not a final delivery | Same effects as `withdraw`: the task goes back to in progress (while #317 keeps clarifications). |
| F-GH-5 · post-merge work and an idle node | Enforce the documented rule: after the merge, a turn that ends with nothing running and without declaring the work again ends the post-merge work. |
| Cancelling while `gh pr merge` runs | Accept the cancellation and say on its outcome that the merge was already in progress. |
| F-ORCH-1 · a batch cut by a daemon restart | Alondra resumes alone: confirmed effects are handed off, a batch with no effect is offered again, and an uncertain operation goes to the next batch as evidence. No card for the user. |
| F-ORCH-2 · reminders of a replan the orchestrator does not decide | Remind only on rests caused by other work; after K reminders the task shows the factual state «waiting for a decision of the orchestrator» and no more batches until something changes. |
| F-PROV-1 · a usage reading and a limit rejection disagree | The latest measurement wins, by measurement time (not arrival time). |
| F-PROV-2 · an expired order the host never had | Typed `expired` rejection; the input goes back to the queue without a card. |
| Engine deadlines outside `obligation_policy` (F-ENGINE-5, F-ENGINE-1) | `opening_deadline` and `SILENT_HOSTS` move into the policy, with measure and reason; `UNOWNED_TAKE_EVERY` stays a cadence; `IN_FLIGHT` stops deciding slots and only feeds the memory estimate. |
| F-ENGINE-3 · pause of a queued task | A domain fact, like node pauses; cancelling the task clears it. |
| Historical account moves left in `Admitted` (M-ENGINE-6) | Closed as `Undone` in a migration instead of retrying `finish` every 10 s. |
| F-STORE-1 · `--test-service-push` while the daemon runs | Sent by the live daemon through its API. |
| F-STORE-3 · manual retry of an orchestrator message older than 24 h | Refused with a clear message instead of «retryable». |
| F-SERVER-D1 · ambiguous first relay enrolment | Retry the safe failures, compensate the ambiguous ones (delete, then retry with the same id); whatever stays unresolved shows «pair again» on the device list and the phone. |
| F-SERVER-D4 · revoking the Mac's own terminal | Not allowed (it protects nothing: local clients already use the loopback path). |
| F-SERVER-D7 · self-update retries | 3 retries per version with a growing wait, then stop until the next version, shown in the status. |
| Relay: a Mac that stops reporting `active` | A report older than `STALE_AFTER_MS` no longer starts or rotates the Live Activity; it is still shown as stale, with no new alerts. |
| Relay: invitation token lost on an idempotent retry | The interface offers «reissue» on that error; the secret is never stored. |
| D-R1 · quitting the TUI with unconfirmed replies | Ask for confirmation on a manual quit; on the automatic restart, keep them on disk and resend them. |
| D-R2 · safe retries of MCP mutations without a card | A stable key from the arguments' digest and the task, for the kinds the daemon lists as naturally idempotent. |
| D-R3 · a local terminal removed by hand | Pair it again once and say so. |
| D-A1 · switching to a Mac that does not answer | Always switch and show that Mac's connection state (with its saved home, issue #386). |
| D-A2 · saved notification replies for a Mac that is not selected | Delivered in the background through that Mac's connection as soon as it answers. |
| D-A3 · Tasks after 30 s without confirmation | The row or button comes back with the notice, like the queue and the Mac. |

## Task and node lifecycle (`LIFE`)

Scope: the task lifecycle aggregate and the node it runs on, and everything the visible state derives from them. Real paths (the the Bazel layers work phases moved files): the aggregate is in `crates/domain/src/lifecycle*.rs`; the bridge to the rest of the domain is `crates/domain-rules/src/lifecycle.rs`; `Situation` is in `crates/domain-rules/src/situation.rs` (not in `crates/core`, as the doc says); the inherited "node state" lives in `crates/core/src/{states.rs,tasks.rs,tasks/derive.rs}` and is used in memory by `crates/daemon-engine/src/live.rs`. Audited commit: `1567967b9`. Read-only audit: no build or test was run.

### M-LIFE-1 · `TaskLifecycle`
- **Kind**: explicit, composite. **Defined in**: `crates/domain/src/lifecycle.rs:1099-1196` (`Progress`, `DeliveryGate`, `Delivered`, `Finishing`, `State`, `TaskLifecycle`); transitions in `lifecycle/commands.rs:4-568` (`transition`) and `lifecycle/observations.rs:4-719` (`runtime_fact`); `advanced` in `lifecycle.rs:1668-1704` (copies, applies, bumps the revision, sets `owed_since`).
- **Source of truth**: the task journal (`DomainState.lifecycle_journal: Vec<LifecycleRecord>`, persisted in `lifecycle_events`, `crates/store/src/schema.sql:13-19`). The aggregate is rebuilt by replay (`domain-rules/src/lifecycle.rs:1240-1247`, `lifecycle_of`) and cached in `DomainState.lifecycles` (Arc; used only if `revision+1 == journal.len()`).
- **Copies and derivatives**: projections the store writes: `lifecycle_tasks.revision`, `lifecycle_current_execution`, `lifecycle_cards`, `lifecycle_dependencies`, `lifecycle_delivery` (`crates/store/src/persistence.rs:191-245`). Copies in the `tasks` row (see M-LIFE-6).
- **States** (`State`):

| State | Content | Notes |
|---|---|---|
| `Unstarted{input, remaining}` | no execution | |
| `InProgress(Queued{stage,input,remaining})` | no execution | |
| `InProgress(Executing{stage, ActiveExecution})` | execution present, `Available` or `Reserved+Launching` | `ActiveExecution::new` rejects `Resting` (`lifecycle.rs:1063-1077`) |
| `InProgress(Waiting{stage, Decisions≠∅, Session})` | non-empty cards by kind (`Decisions`) | |
| `InProgress(Dependency{stage, tasks≠∅, decisions, Session})` | | **unreachable in production**: no record produces `WaitForDependencies` (see F-LIFE-1, last section) |
| `Delivered{delivery, gate, decisions, Session}` | `gate ∈ {Review, Blocked, AwaitingMerge, Accepted}` | `Blocked` and `AwaitingMerge` are **unreachable in production** (no `BlockDelivery` record, no `Accept{merge:Some}`) |
| `Finishing{intent, remaining≠∅, Session}` | drain obligations | |
| `Completed` / `Cancelled` | terminal | |

- **Composite state**: `State × Execution (M-LIFE-2) × plan_reviews × post_merge × doubts/settled × empty_turns/woken`. The fields outside `state` (`plan_reviews`, `post_merge`, `doubts`, `settled`, `cancelled_through_generation`) survive `Reopen`.
- **Events** (`Event`, `lifecycle.rs:513-712`): Admit, Runtime{stamp,cursor,fact}, Ask, Answer, Decide, MoveCard, KeepQuestion, PlanApproved, HostReleased, HostRetaken, BlockDelivery, DeliveryUnblocked, Present, Accept, MergeObserved, HandBack, WaitForDependencies, Tell, DependenciesResolved, Pause, Resume, EndPause, RecoveryEnded, Finish, DrainObserved, PostMergeDeclared, PostMergeEnded, RetireReleasedExecution, ExecutionReleased, ExecutionLeftForReview, Reopen, HoldForPlanReview, PlanReviewSettled, DoubtHandoff, ResendUntaken, ResendDoubtful, SettleHandoff, Discard, AwaitEnded, Unfinished, RequestControl, Compact.
- **Events produced by `domain-rules`** (mapping `event()`, `domain-rules/src/lifecycle.rs:616-803`): all except `Ask`, `PlanApproved`, `BlockDelivery`, `DeliveryUnblocked`, `MergeObserved`, `Accept{merge:Some}`, `WaitForDependencies`, `DependenciesResolved`, `RetireReleasedExecution`, and the facts `ConnectionLost`, `ResourceReleased`, `Notification`, `EffectAccepted`, `EffectObserved` (checked with `rg`: 0 occurrences in `domain-rules/src/{lifecycle.rs,rules.rs,rules/}`). These are aggregate branches that only its own tests exercise.
- **Allowed transitions (essential)**:

| From | Event [guard] | To / effect |
|---|---|---|
| Unstarted, Queued, Delivered{Absent, gate≠Blocked}, Waiting{Absent, only Unheard/Recovery} | Admit [no `plan_reviews`, no pause] | Executing{Reserved, Launching(input)} (Delivered stays `Delivered` with a session); `generation+1` |
| any state with a current execution | Runtime [stamp == current, cursor == current+1] | depends on the fact (M-LIFE-2) |
| Unstarted/Queued/Executing/Waiting/Dependency/Delivered | Pause [owner≠Gate, no other pause], Ask, DoubtHandoff | Waiting (or the same Delivered/Dependency with the card added) |
| Waiting/Dependency/Delivered | Answer/Resume/EndPause/Decide [card exists, right kind] | removes the card; if none are left: Queued/Executing (`ActiveExecution::new`) |
| Executing/Waiting/Dependency | Present | Delivered{Review} (Waiting drops the orchestrator's cards) |
| Delivered{Review} | Accept{merge:None} | Delivered{Accepted} |
| Delivered{Review} | HandBack | Executing/Waiting/Queued with stage ChangesRequested |
| not terminal and not Finishing | Finish [Complete: gate Accepted, no cards except Recovery, no pending inputs] | Finishing{Cleanup + PostMerge + Execution + Work + Callback + Effects + Merge} |
| Finishing | DrainObserved [obligation present] | removes it; the last one → Completed/Cancelled (`cancelled_through_generation = generation`) |
| Completed/Cancelled | Reopen | Queued{Implement} |
| Executing/Waiting/Dependency/Delivered with an execution | ExecutionReleased / ExecutionLeftForReview [no work, callbacks or effects] | Session::Absent with what was not consumed |

- **Rejected**: every `Err(Refusal::…)` is typed; `apply` leaves the aggregate untouched (`lifecycle.rs:1662-1666`). In `domain-rules` a refusal becomes a `DomainError` (`refused`, `lifecycle.rs:3198`) and aborts the whole command. Facts on terminal states: `runtime_fact` returns `StaleExecution` because there is no session (`observations.rs:10`). In `Finishing`, every fact other than `HostEnded/HostStopped/TakenOver/TakeoverUnanswered` is taken "unchanged" (only the cursor advances, `observations.rs:21-24`).
- **Terminals**: `Completed`/`Cancelled`. Only `Reopen` reopens them, and only two explicit commands emit it (`Command::ReopenTask`, `rules.rs:5296-5315`; adoption of the PR of a closed task, `rules.rs:5260-5294`). No late provider fact or answer reopens them: without a session `runtime_fact` refuses and `answer()` refuses (`commands.rs:684-698`). `ReopenTask` can re-admit the old node with a synthetic `claude --bg` execution (`Admitted{execution:None}` + `ResourceAvailable`, `rules.rs:5310-5313`).
- **Invariants**: enforced by type: `Decisions` non-empty, `NonEmpty` in dependencies and drain, `Several` in `LiveState`, `ActiveExecution` (not `Resting`), `TaskLifecycle` without `Clone`/`Deserialize` (`compile_fail` doctest). Only assumed: that `Executing` never ends up with `Activity::Resting` (imposed by the `runtime_fact` paths and `ActiveExecution::new`, not by the type; `Tell` on `Executing` mutates `execution.0` without revalidating, `commands.rs:174`).
- **Persistence and restart**: the journal is the truth; after a restart `load_journal` rebuilds, validates each task with `agrees` (`domain-rules/src/lifecycle.rs:1351-1480`), and loading fails if a row does not match. The aggregate's cursor is recomputed on replay (`event()` always uses `current cursor + 1`, `lifecycle.rs:617-620`): the domain journal stores neither the provider's cursor nor its stamp; deduplication of repeated facts lives upstream (cursor of `provider_observations`, see M-STORE-5) and in `turn_end_recorded` for turn ends.
- **Concurrency**: the aggregate is pure; only `DomainState::decide` mutates it, inside a command, and the store serializes commands (out of scope, see STORE).

### M-LIFE-2 · Execution (`Execution`)
- **Kind**: explicit, product. **Defined in**: `lifecycle.rs:921-1061`; mutations in `observations.rs` and helpers `queue_input`, `add/remove_callback_wake`, `replace_handoff` (`lifecycle.rs:1806-1876`, `commands.rs:1114-1132`).
- **Components**: `Activity ∈ {Launching(input), InputPending(input), Turn{id, work, continuations, taken, stop}, OwnWork{work≠∅, continuations}, Continuation(wakes≠∅), Awaiting(Await), Resting}` × `ResourceEvidence ∈ {Reserved, Available, ObservationPending, Released}` × `Takeover ∈ {None, Pending, Unanswered}` × `host_released` × `callbacks: Vec<CallbackState{AwaitingAnswer, Answered, Decided}>` × `effects` × `inbox` × `held` × `cursor`.
- **Facts** (`RuntimeFact`, `lifecycle.rs:775-871`) and key effects:

| Activity | Fact [guard] | Result |
|---|---|---|
| Launching (Reserved) | ResourceAvailable / any live-session fact | InputPending, Available |
| InputPending(i) | TurnStarted{Input(i)} | Turn |
| any ≠ Turn(id) | unexpected TurnStarted | `provider_turn_started`: opens Turn, the expected input goes back to the queue; `unprompted` drops Unheard cards |
| Turn(id) | TurnStarted(id) | unchanged (repeated) |
| Turn | WorkStarted | adds work |
| Turn / OwnWork | WorkEnded [it runs it] | removes work, adds a continuation; empty OwnWork → Continuation |
| Turn | TurnEnded | OwnWork / Continuation / Awaiting(await) / Resting; Resting in Executing → wake (≤3 in a row), resume, or Unheard card |
| ≠ Turn | TurnEnded | opens an "unseen" turn and closes it (never refused) |
| Reserved/Available | HostEnded / HostStopped | cuts Turn/OwnWork/Continuation → Resting (or Awaiting(Instruction) if the user stopped it without a pause); callbacks drained |
| Turn{stop:Running} | InterruptAccepted | stop = InterruptAccepted |
| takeover None/Unanswered | TakenOver | Pending; **Pending → `RepeatedTakeover` (refusal)** |
| takeover Pending | TakeoverUnanswered | Unanswered; otherwise `InvalidState` (refusal) |

- **Rejected**: facts with correct stamp and cursor that are refused, contradicting rule 3 of the doc ("a provider fact is never refused"): `Ask` with a reason other than Question/Permission, repeated `TakenOver`, `TakeoverUnanswered` without `Pending`, `ConnectionLost` with resource `Released`, `TurnEnded` with `awaits: Tasks` that includes the task itself, duplicate card in `Waiting` (`DuplicateObligation`). See the last section.
- **Persistence and restart**: inside the journal (M-LIFE-1). `held`, `takeover`, `host_released` are rebuilt by replay.

### M-LIFE-3 · Root turn stop (`TurnStop`)
- **Kind**: explicit. **Defined in**: `lifecycle.rs:311-316`.
- **Transitions**: `Running → InterruptAccepted` by fact; `InterruptAccepted → ForceRequested` by `RequestControl{ForceStop}` (`commands.rs:534-546`).
- **Rejected**: `ForceStop` on `Running` → `InvalidState`; repeated → `DuplicateObligation`.
- **Persistence and restart**: lost when the turn ends (it is a field of `Activity::Turn`).

### M-LIFE-4 · Empty-turn counter
- **Kind**: implicit. **Defined in**: `empty_turns: u8`, `woken: Option<InputId>` (`lifecycle.rs:1191-1194`); rises in `wake_with`, resets in `rest_wakes` except for the `keeps_wake` events (`lifecycle.rs:1707-1735`). Limit `EMPTY_TURN_WAKES = 3`.
- Decides whether an empty turn end wakes the node or opens the orchestrator case (`hands_back_an_empty_turn`, `lifecycle.rs:1230-1242`; `domain-rules/src/lifecycle.rs:1598-1645`).
- **Source of truth**: the aggregate alone (rule 4 of the doc holds for the count).

### M-LIFE-5 · Obligation (`obligation()`)
- **Kind**: derived. **Defined in**: `lifecycle/obligation.rs:53-161`, `Party × Owed`, `since = owed_since` (revision at which it changed).
- **Total**: for every non-terminal state it returns `Some`: `Waiting` carries non-empty `Decisions` and `waits_on` covers the three `Owner`s; `Delivered` falls to `gate`; `Executing` falls to `by_activity` (which covers every `Activity`). `Completed/Cancelled` → `None`. Checked, no gaps.
- **Adjustments outside the aggregate**: `DomainState::obligation_with` (`ownership.rs:329-342`) changes the obligation to `Answer` (returned delivery waiting for the orchestrator) or `Replan` (cancelled dependency / rejected plan): the aggregate never produces `Owed::Replan`.
- **Deadlines**: `obligation_policy.rs` (`HAND_INPUT` 5 min, `WAKE` 2 min, `ANSWER` 10 min), applied by `obligation_watch` (out of scope: ENGINE).

### M-LIFE-6 · Task row `tasks.state` and flags
- **Kind**: implicit, derived with validation. **Defined in**: `domain-rules/src/model.rs:123-145` (`TaskState` with 9 values), `Task` (`started`, `agent_id`, `final_presentation`, `ending`, `wrap_up`, `pending_user_inputs`, `waiting_for_tasks`, `dependencies`, …).
- **How it is written**: (1) `materialize` after every lifecycle decision sets `agent_id`, `started`, `ending` and the phase's `TaskState` (`domain-rules/src/lifecycle.rs:1314-1336`); (2) `derive_states` / `state_within_phase` picks the attention state within the phase (`rules.rs:2614-2662`, `lifecycle.rs:2613-2650`); (3) `derive_loaded_states` keeps `WaitingDependency` on open (`rules.rs:2626-2636`).
- **Validation**: `agrees` (`lifecycle.rs:1413-1480`) for every task "touched" in the commit (`rules.rs:3012`) and on load: node/start/ending equal to the execution; phase ↔ state compatible; required cards actionable.
- **Copies that can diverge**: `dependencies`/`waiting_for_tasks` (row) against `Progress::Dependency` (aggregate, dead) and `Await::Tasks` (aggregate, only after a turn): the dependency wait of an unstarted task exists only in the row (F-LIFE-1). `wrap_up.post_merge_work` (row) against `post_merge` (aggregate): synchronized in `declare_post_merge`/`end_post_merge`/`adopt_declared_post_merge` (`lifecycle.rs:2441-2491`).
- **Terminals**: `Done`/`Cancelled` only from the phase; `derive_states` does not touch terminal rows.

### M-LIFE-7 · `Situation` and `Tally`
- **Kind**: derived. **Defined in**: `domain-rules/src/situation.rs:7-226`; computed by `ReadIndex::build` per task (`read_index.rs:614`) with `StartProblems::situation` → `situation_with`.
- **Reads**: `task.state` (terminals), the inbox listing, `task_activity` (closed node), `node_closing`, `obligation_of` (M-LIFE-5), cards and `awaits` of the aggregate, `answered_wait`, `task_handed_back`, `finishing_intent`. `Tally` decides block and counters (`situation.rs:68-82`, `read_index.rs:99`, `views/src/lib.rs:85-93`, `views/src/tasks.rs:530-540`).
- **Real precedence**: terminal row → inbox `needs_you` → closed node (`Waiting(Slot)`) → delivered and node closing → turn/work → user pause or `Await::Instruction` → user cards → delivered phase → cancellation in progress → wait for an obligation.

### M-LIFE-8 · `LiveState` / `TaskActivity` / node state in `/runtime`
- **Kind**: derived. **Defined in**: the aggregate's `LiveState` (`lifecycle.rs:1499-1530`), adjusted by `DomainState::live_state` (`domain-rules/src/lifecycle.rs:1848-1866`) and wrapped in `TaskActivity` (`node_machine.rs:226-270`).
- **Copies and derivatives**: `ReadIndex` keeps `situation`, `activity` and `live` per task side by side (`read_index.rs:615-619`). `daemon-engine/src/live.rs:403-424` translates `sdk_live` into the inherited model's `NodeState`/`NodeStatus` for the node list: every `Working(Alondra(_))` (starting, delivering, waking, waiting for the orchestrator, merging, finishing) comes out as `Working`/`Busy`, while `Situation` treats it as waiting (see H-LIFE-3).

### M-LIFE-9 · Node phase (`NodePhase`) and node pause
- **Kind**: explicit. **Defined in**: `node_machine.rs:104-190`, `node_next` (`:340-373`).
- **States**: `NodePhase ∈ {ThroughSdk, Closed}` (any old value reads as `Closed`). `Pause{reason, since, cut, turn_since, cleanup ∈ {Pending, Terminated, Done}, released}`.
- **Events**: `SessionClosed`, `Paused`, `PauseCleanup`, `PauseReleased`, `Resumed`. The node's pause is mirrored as a `Pause` card in the aggregate (`hold_node_pause`, `:607-613`); its end decides `PauseEnded` or, if the aggregate refuses or the task would be left stranded, `Resumed` with text (`end_of_pause`, `:579-589`).
- **Terminals**: `Closed` is terminal for the node; `node_next` rejects everything except a repeated `SessionClosed`.

### M-LIFE-10 · Node closing / wrap-up
- **Kind**: implicit. **Defined in**: `Task.wrap_up` (`asked_at`, `cleaned_at`, `node: Option<NodeEnd>`, `post_merge_work`, `close_cancelled`) + `node_closing()` (`rules.rs:2540-2546`: terminal, `wrap_up.node` or confirmed cleanup) + `in_cleanup`, `wraps_up`.
- Governs rule 8 of the doc (nothing asks, nobody resumes it). `RecordNodeEnd` drains everything at once if the task ends (`rules.rs:5409-5414`), or releases the execution if not.

### M-LIFE-11 · Held plan review
- **Kind**: implicit, in the aggregate. **Defined in**: `plan_reviews: BTreeMap<InteractionId, u64>` + `plan_revision` (`lifecycle.rs:1178-1183`).
- `HoldForPlanReview` only before admission; `Admit` is refused while any is held (`commands.rs:12-14`). Outside `state`: survives `Finish` and `Reopen`.

### M-LIFE-12 · Doubtful deliveries
- **Kind**: implicit, in the aggregate. **Defined in**: `doubts: card → input`, `settled: set<input>` (`lifecycle.rs:1185-1189`).
- Events `DoubtHandoff`, `SettleHandoff`, `ResendUntaken`, `ResendDoubtful`; `Finish{Cancel}` moves the doubts to `settled`.

### M-LIFE-13 · Inherited reducer of `crates/core`
- **Kind**: explicit, in memory. **Defined in**: `core::tasks::TaskState` (8 values, `tasks.rs:74-122`) with the matrix `Board::apply_fact` (`tasks.rs:749-773`) and `core::states` (table `NODE_TABLE` of node states by hook evidence).
- **Source of truth**: lives only in memory in `Live.view` (`daemon-engine/src/live.rs:31-37`) for nodes seen through hooks (`claude --bg`); SDK nodes are overwritten from the lifecycle (`from_lifecycle`). A parallel source of truth only for non-SDK nodes; `client-port/src/i18n.rs` still imports its enums.
- **Persistence and restart**: not persisted; rebuilt from hooks after startup.

### M-LIFE-14 · `crates/machine-model` (model test; not a machine)
- Models, with the real domain: deliveries, questions to the user and to the orchestrator, permissions, background work, PR, empty turns, host death and stop, late and repeated facts (deduplicated by the model's own cursor, `lib.rs:423-447`), restarts, slots, memory, disk, usage, account move, priority, cleanup.
- **Does not model** (0 occurrences in `lib.rs`): interrupt and `ForceStop`, `ReopenTask`, `HostReleased/Retaken`, `InputDropped`, `InputHeld`, `Unprompted` turns, compaction, held plan review (`SettlePlanReview`), `WaitForTasks`, Codex, `RecordClaudeHold`, `DecisionAborted`, work after the merge (`DeclarePostMergeWork`), merge authorization. Nor tasks created with unstarted dependencies except `history_with_a_dependency`, which checks neither block nor counter (see F-LIFE-1). `KNOWN_GAPS` is empty (`tests.rs:14`), and the doc says "no known gaps" while admitting that it reimplements the gate and the host owner.

### Findings

#### F-LIFE-1 · An unstarted task that waits for other tasks shows as "Queued" (slot wait), not "Waiting for tasks"
**Class**: CONFIRMED · **Severity**: S2 (wrong visible state and counters; nothing is lost) · **Probability**: high (every task created with `dependencies`, e.g. stacked phases, goes through this while its dependency has not landed) · **Overlap**: no task in the plans of related work and no open issue covers it (#323 is about the outcome of dependents when one is cancelled, not their visible state). `git log` of `situation.rs`: no related changes since #297. The evidence is new.
- **Invalid state/transition and broken invariant**: rule 1 of `task-states.md` ("`Situation` … nothing else decides a label, a block, a count") and the definition of `Tally::Waiting` ("It waits for something else: other tasks", `situation.rs:63-66`). There are two sources of truth for "waits for tasks": the row (`TaskState::WaitingDependency`, `dependencies`) and the aggregate. The aggregate does not know it before the first turn, so `Situation` says something different from the row.
- **Evidence**:
  1. `LifecycleRecord` has no variant for `WaitForDependencies`/`DependenciesResolved` (`domain-rules/src/lifecycle.rs:294-494`); `event()` cannot produce them (`:616-803`). `Progress::Dependency` (`domain/src/lifecycle.rs:1116-1121`) is unreachable in production.
  2. Creating the task only records `Planned` (`plan_lifecycle`, `domain-rules/src/lifecycle.rs:1276-1283`) → `Unstarted`.
  3. The row does become `WaitingDependency`: `state_within_phase` → `waits_are_over(t).is_err()` (`lifecycle.rs:2625-2626`, `rules.rs:1974-1990`).
  4. `owes()` for `Unstarted` without plan reviews → `Room(Gate)` (`domain/src/lifecycle/obligation.rs:62-66`); `obligation_with` changes it to `Replan` only if the dependency is cancelled or the plan was rejected (`ownership.rs:329-342`).
  5. `situation_with`: no cards, no node, not delivered → `Waiting(match Room(Gate) => WaitsFor::Slot)` (`situation.rs:205-226`).
  6. `Tally`: `Waiting(Slot)` → `Tally::Queued` (`situation.rs:76`); block "Queued / En cola" (in queue) and the `queued` counter in the header and the Live Activity (`views/src/tasks.rs:530-540`, `read_index.rs:99`, `views/src/lib.rs:85-93`).
  7. The detail does say "Waits for “X”" because `slot_fact` falls into `chained` (`views/src/task_state.rs:339-375`, `:588-604`): the row sits in the queue block with text about waiting for another task.
- **Minimal sequence**: 1) `CreateTask{a}`; 2) `CreateTask{b, dependencies:[a]}`; 3) build `ReadIndex`: `b.situation == Waiting(Slot)`, `tally == Queued`, while `tasks[b].state == WaitingDependency`.
- **Impact**: the "Queued" block and the "queued" number in the header/Live Activity include tasks that wait for another task, not for a slot; the "Waiting" block omits them. With several stacked phases the user sees a long queue that does not move when slots free up.
- **Deterministic test and ablation**: a domain test in `crates/domain-rules/src/situation_tests.rs` (or `views/src/tests.rs` for block and counters) with two tasks created by commands, no clock: assert `Situation::Waiting(WaitsFor::Tasks)` and `Tally::Waiting` for `b`. Today it fails with `Waiting(Slot)`. Ablation: remove the fix and see the same failure.
- **Expected fix and files**: the task-wait of an unstarted task must be known by the source `Situation` reads: either the aggregate (record `WaitForDependencies`/`DependenciesResolved` from `derive_states`/`ReviewWaits`, which also stops `Progress::Dependency` being dead code), or `obligation_with` returns `Owed::Tasks` when the row is `WaitingDependency` and unstarted. Files: `domain-rules/src/{ownership.rs,lifecycle.rs,rules.rs}`, new test.

#### F-LIFE-2 · Reopening a task whose SDK node the user left open creates a `claude --bg` execution nobody runs
**Class**: CONFIRMED in the domain (the part of the engine that leaves the task idle is H-LIFE-4) · **Severity**: S2 (the task stays "Delivering message" with no host accepting its facts; cancelling it is the way out) · **Probability**: low (needs "Leave it open" on the closing card and then `ReopenTask`) · **Overlap**: none in the plans of related work or in issues. `git log -S` of that line: no changes since #297. The evidence is new.
- **Invalid state/transition and broken invariant**: "No node runs on `claude --bg` any more" (`engine-sdk/src/bg_handover.rs:1-3`): the domain still admits new `claude --bg` executions, and does so on a node that runs through the SDK, so the task's current execution and the node's real host diverge.
- **Evidence**:
  1. The closer records `NodeEnd::KeptByUser` when the user chooses to leave the node (`engine-sdk/src/task_closer.rs:325`); neither that path nor `cleanup.rs:229` closes the session, so `agents[a].phase` stays `ThroughSdk` (alive).
  2. `ReopenTask` (`rules.rs:5296-5315`): `released` is false (node alive and no other task), `node_gone` only looks at `Closed|Gone`, so `kept = Some(a)` and `Admitted{node: a, execution: None}` + `ResourceAvailable` is decided.
  3. `execution: None` becomes a host stamp `claude-bg` (`domain-rules/src/lifecycle.rs:623-628`).
  4. From then on every SDK host fact of `a` is refused: `current_sdk` requires the current generation and a host other than `claude-bg` (`lifecycle.rs:2723-2731`), used by `record_runtime_facts`, `record_turn_boundary`, `record_host_ended`, … (`:1496`, `:1527`, `:1988`, `:2330`).
  5. `admitted_through_sdk` becomes false (`read_index.rs:906-911`), so the task does not pass through the gate (`node_machine.rs:400-411`); `task_activity` still reads it as `Live` because the node phase is `ThroughSdk` (`node_machine.rs:260-269`) → `Working(Alondra(Delivering))` → `Situation::Waiting(Step(Delivering))`.
  6. `ReopenTask` emits no `NotifyAgent`: nobody tells the node the task is back.
- **Minimal sequence**: 1) an SDK task (`AdmitExecution`) delivers and is accepted; 2) closing: the user answers "Leave" → `RecordNodeEnd{KeptByUser}` → `Done`; 3) `ReopenTask` → `claude-bg` execution of generation g+1 in `InputPending`; 4) any `RecordRuntimeFacts{generation: g}` → `stale_execution`; with g+1 → `invalid_input`.
- **Impact**: the row says "Delivering" forever; the node's facts are discarded; the `HandInput` deadline does not act (no recorded exit and no own host, `obligation_watch.rs:144`, `:263-266`).
- **Deterministic test and ablation**: a domain test in `crates/domain-rules/src/tests.rs`, an SDK variant of `a_task_reopened_with_its_node_kept_by_the_user_keeps_working_with_it` (`tests.rs:4111-4121`, which today only covers `claude --bg`): admit through `AdmitExecution`, close with `KeptByUser`, reopen, and check that the current execution is not `claude-bg` (or that the task is left without a node, depending on the product decision) and that a `RecordRuntimeFacts` from the node is accepted. Today it fails on the host assert.
- **Expected fix and files**: never admit `execution: None` for an SDK node. Either the reopened task is left without a node (as with `Closed|Gone`) and the gate starts a new SDK execution that resumes the session, or `AdmitExecution` is recorded with the SDK identity of the kept node. Files: `rules.rs` (`ReopenTask`), test.

### Hypotheses

#### H-LIFE-1 · A repeated `TurnStarted` after its `TurnEnded` reopens the turn and its end is swallowed: "Working" with no turn
What is claimed: the domain journal stores neither provider cursor nor stamp (`event()` always uses "current cursor + 1" and the current stamp, `domain-rules/src/lifecycle.rs:617-620`), so the aggregate cannot refuse a repeated fact. A `TurnStarted{X}` with the activity in `Resting/Awaiting` opens a new turn (`observations.rs:225-227`, `provider_turn_started`). The next `TurnEnded{X}` is ignored because `turn_end_recorded` already sees an end of X since the last `Admitted` (`lifecycle.rs:1560-1564`, `:1590-1592`). Result: `Activity::Turn` with no end → `Owed::Turn`, `Situation::Working(Turn)`, no deadline, until the host dies.
Missing to confirm: that a restart can re-deliver already-applied facts to the domain, that is, whether the `RecordRuntimeFacts` commit and the advance of the normalization checkpoint (`provider_normalization_checkpoints`) are atomic (STORE/ENGINE). The model (`machine-model/src/lib.rs:423-447`) deduplicates with its own cursor, which `restart` does not clear (`:383-395`), so it does not exercise this. Proposed test: a scenario with host doubles that re-emit the batch after a simulated restart between commit and checkpoint; or a domain test applying `TurnStarted{X}`, `TurnEnded{X}`, `TurnStarted{X}` and checking the task does not read `Working`. Overlap: scope of the stalled-node recovery work (phase 2, `StreamEndedMidTurn`/contradictions): new evidence for them, not a fix of its own.

#### H-LIFE-2 · `finish_settled_deliveries` can refuse the command that settles a delivery with an open orchestrator case
What is claimed: it runs in the post-processing of **every** command (`rules.rs:2951`) and its error aborts the command. Its filter (`lifecycle.rs:2569-2578`) looks at `DomainState` predicates (`unanswered` excludes system cards and `blockers` excludes the orchestrator's, `rules.rs:2301-2355`), but the aggregate's `finish()` refuses `Complete` if any card other than `Recovery` is left (`commands.rs:1045-1053`). An orchestrator `Unheard` card (stopped-node case, created by `Unfinished` in `Delivered`, `commands.rs:899-923`) passes the filter and makes `finish` fail.
Missing to confirm: a real sequence in which the delivery is settled (accepted or merged) with that case open and no node (`!wraps_up`): e.g. open case → the session dies (`RecordNodeEnd{Gone}`, which releases the execution and keeps the `Delivered` cards, `commands.rs:1187-1192`) → the user accepts. If reached, `AcceptDelivery` (or the merge observation, or a command of another task that makes this one "completable") is refused with `lifecycle refused` until the orchestrator settles the case. Proposed test: a domain test with that sequence; assert the command that settles the delivery is accepted and the task ends or the case is withdrawn. Overlap: ORCH (`Answer` deadline) and phase 4 of the stalled-node recovery work.

#### H-LIFE-3 · The node list (`/runtime`) says "Busy" while the task says it waits
`from_lifecycle` (`daemon-engine/src/live.rs:403-424`) translates every `LiveState::Working(_)`, including `Working(Alondra(Answering|Starting|Delivering|Waking|Merging|Finishing))`, into `NodeState::Working`/`Busy`, while `Situation` reads it as waiting (`situation.rs:239-247`) and the doc says an Alondra step "is not work". Missing: confirm which screen shows the `/runtime` node state today (the Fleet screen is being retired). S3.

#### H-LIFE-4 · The task of F-LIFE-2 does not leave "Delivering" by itself
Missing: confirm in the engine that no loop picks up a `claude-bg` execution in `InputPending` of a `ThroughSdk` node (gate: `passes_gate_in` requires `admitted_through_sdk`; `obligation_watch`: `Requeue` only if `hosts().owns(agent)`; `bg_handover` only advances handovers already recorded, `bg_handover.rs:235-242`).

### Docs that differ from the code
Differences between `docs/architecture/task-states.md` and the code:
1. "`Situation` in `crates/core`": it is in `crates/domain-rules/src/situation.rs`.
2. Table "Who owes the next step": `Replan` is missing, and `obligation()` is not the only authority: `obligation_with` (`ownership.rs:329-342`) rewrites the obligation to `Answer`/`Replan` outside the aggregate.
3. Rule 3 ("a fact of the provider is never refused … only another execution, a repeated fact or a gap"): the aggregate refuses facts with correct stamp and cursor (`RepeatedTakeover`, `TakeoverUnanswered` without `Pending`, `ConnectionLost` with `Released`, `Ask` with an invalid reason, `TurnEnded` with `awaits` that includes itself, card id collision; `observations.rs:83-85`, `:360-362`, `:472-482`, `:343`, `:588-592`), and `record_runtime_facts` aborts the whole batch on any of them or on `TakenOver` of a node that is closing (`domain-rules/src/lifecycle.rs:2329-2379`). Also, in production the aggregate never sees a foreign cursor or stamp: `event()` fabricates both, and the real protection is `current_sdk` (generation), the store's cursor and `turn_end_recorded`.
4. Rule 4 ("a caller never tries one variant of an event and falls back to another when it is refused"): `end_of_pause` tries `EndPause` and falls back to `Resumed` if refused (`node_machine.rs:579-589`); `record_host_ended` tries `HostEnded` with `resume` through `sends()` before deciding (`lifecycle.rs:2021`, `:1305-1310`).
5. `Situation` precedence: the code has two steps the doc does not list (closed node → `Waiting(Slot)` before the lifecycle; delivered with node closing → `Delivered(Closing)` before the live turn) and shows `Cancelled` while a cancellation is still draining (`situation.rs:153-159`, `:202-204`).
6. Item 14 ("The model test … no known gaps"): the model does not cover interrupt/`ForceStop`, reopen, host release and recovery, `InputDropped`, `Unprompted` turns, held plan review, work after the merge, Codex (M-LIFE-14). F-LIFE-1 and F-LIFE-2 are outside what it checks.
7. Deadlines/timers: see F-ENGINE-5 (already recorded by ENGINE); not duplicated here.
8. The aggregate has branches production never reaches (`Progress::Dependency`, `DeliveryGate::Blocked/AwaitingMerge`, `RetireReleasedExecution`, `ConnectionLost`, `ResourceReleased`, `EffectAccepted/Observed`, `Notification`, `PlanApproved`, `Event::Ask`): the doc and the tests describe a bigger machine than the one that runs.

## Cards, plans, documents and requests (`CARDS`)

Scope: the interaction machines and the work machines of the domain, outside the turn/node lifecycle. Code: `crates/domain-rules/src/` (mainly `rules.rs`, `guards.rs`, `model.rs`, `card_contract.rs`, `rules/plan_review.rs`, `rules/snooze.rs`, `rules/open_pr.rs`, `ownership.rs`, `projection.rs`, `attention.rs`), `crates/engine-orchestrator/src/orchestrator_inbox.rs` (only where it closes requests), `crates/store-sqlite/src/lib.rs` (schema) and `crates/store-sqlite/src/command.rs` (effect outbox). Audited commit: `1567967b9`.

Execution model common to all machines in this area (stated once):

- **Source of truth**: `DomainState` (`model.rs:1416-1508`) persisted in `domain.db` (tables `interactions`, `requests`, `tasks`, `task_waits`, `task_checklists`, `delivery_checklists`, `request_plans`, `plan_reviews`, `request_folds`, `task_creations`, `cleared_requests`, `task_post_merge_work`, …; `store-sqlite/src/lib.rs:79-216`). The SQL schema only enforces foreign keys, `json_valid` and some ranges; **no card invariant lives in SQL**: all of them are in `DomainState::validate` (`rules.rs:5886-6260`), which runs after every command and on load.
- **Transition**: `prepare_transition` (`rules.rs:2926-3028`) clones the state, applies the command (`apply`, `rules.rs:3029-5745`) and then runs a fixed chain of sweeps: `end_lapsed_awaits`, `withdraw_released_stalls`, `withdraw_deliveries_asked_after` (`:2370`), `withdraw_deliveries_behind_their_pr` (`:2395`), `adopt_declared_post_merge`, `finish_settled_deliveries`, `withdraw_cancelled_requests` (`:2271`), `derive_states`, `cancel_orphan_reviewers`, `retire_cards_of_ended_tasks` (`:2473`), `reconcile_snoozed_cards`, `close_finished_messages` (`:1309`), `close_delivered_plans` (`:1368`), effect `RequestTasksOver` (`:2961-2969`), `NotifyUser` of `acting` (`:2970-2975`), `resume_waiting_nodes` (`:1746`), `ended_task_waits` (`:2977-2986`), `wrap_up_step` (`:2549`), `validate`. Everything is atomic (clone-validate-swap).
- **Concurrency**: a single writer; the store applies each command inside an `IMMEDIATE` transaction (`store-sqlite/src/owner.rs:122`) together with its semantic receipt and its outbox (`domain_messages`, `store-sqlite/src/command.rs:290-400`).
- **Freshness**: `admit` (`guards.rs:77-99`) rejects `stale_revision` if `expected_revision` is lower than the revision of the *subject* (`subject_revision`, `rules.rs:2760-2900`): the task for task commands, the interaction for decisions on a card. Every decision on a card raises its `revision`, so two devices deciding on the same card with the same snapshot are serialized: the second gets `stale_revision`.
- **Idempotency**: same `(actor, command_id, epoch)` → the store returns the first result (see M-STORE-2); receipts are deleted by retention. A user/node/orchestrator command that changes nothing is rejected with `no_effect` (`rules/effect.rs:8-35,86-88`); Alondra's (`Actor::System`) may change nothing without error.
- **Restart**: everything in this area is persisted state; nothing lives in memory except the orchestrator's clocks (M-ORCH-6). Effects (`NotifyUser/Agent/Orchestrator`, `RequestTasksOver`, `ReviewDependentPlans`) are written to the outbox in the same transaction and delivered once (M-STORE-1).

### M-CARDS-1 · Life of a card (`Interaction`)
- **Kind**: implicit, composite. State = `withdrawn: bool` × payload (`Information.read_by`, `Question/ActionRequest.answer`, `Delivery{acceptance, is_final, feedback}`) × `task.final_presentation` × `holds` × `snoozed_until` × `clarification`. **Defined in**: `model.rs:906-925` (payload), `:1296-1326` (`Interaction`). Opening: `open()` (`rules.rs:1006-1021`).
- **Source of truth**: `DomainState.interactions` (table `interactions`). **Copies and derivatives**: read index (`read_index.rs`, checked equal in tests with `assert_index_matches`), inbox (`projection.rs:381-405` `inbox_pending`/`card_waits_for_user`), `attention` (`attention.rs:140-189`), apps. **Duplicate that diverges**: `open()` and `card_waits_for_user()` are two definitions of "open"; the inbox also requires an active task, not snoozed, not `out_of_sight`, and for deliveries that `attention` be that card. A system card of an ended task is still `open()` but is not listed (see H-CARDS-2).
- **States**:

| State | Condition |
|---|---|
| Open (question/action) | `!withdrawn && answer == None` |
| Answered | `answer == Some` (final; `ReopenAnswer` only for incomplete groups) |
| Open (notice) | `Information && read_by == ∅ && !withdrawn` |
| Read | `read_by != ∅` |
| Current final delivery | `Delivery{is_final}` and `task.final_presentation == id` and `!acceptance.valid` and `!withdrawn` |
| Accepted delivery | `acceptance.valid` (can be invalidated: goes back to current) |
| Returned delivery | `feedback` with `handed_back`/`HandedBack` in the journal; `final_presentation = None` after `hand_back` |
| Progress (not final) | `Delivery{is_final:false}`: open until accepted or withdrawn |
| Withdrawn/replaced | `withdrawn = true` (`retire`, `rules.rs:1022-1028`) |
| Snoozed | `snoozed_until = Some` (only open inbox cards) |
| Clarification requested | `clarification = Some` (blocks the author's next card, R7) |

- **Events, guards, effects**:
  - Create: `PublishInformation` (`rules.rs:3936`), `AskQuestion`/`RequestAction` (`:3993`), `AskQuestions` (`:4055`), `HoldHook`/`HoldQuestions` (`:4078,4118`, System only), `RaiseProblem` (`:4247`, System or orchestrator), `PresentResult` (`:4811`), system cards (`insert_system_card`, `:842-851`). All go through `insert_interaction` (`:736-841`): new id (`invalid_input` if it exists), `validate_card` (`:113-202`), contract (`NewCard::check`, `card_contract.rs:419-461`), author (`card_author`, `:958-1004`), R8 repeated notice (returns the existing one, `:1073-1084`), R7 pending clarification (`:1086-1108`), R5 one blocking question per node (`:1110-1127`), R15 `blocks` (`:1177-1208`), `replaces` (R6, `:1129-1147`) and slot (R14 `slot_holders`, `:1149-1160`) → `retire` of the replaced ones, R17 no cycles (`:1234-1249`). Effect: `NotifyUser`/`NotifyOrchestrator`.
  - Answer: `AnswerQuestion` (`:4294-4410`): only the recipient; no suggestions or stop cases; already answered → `question_already_answered`; withdrawn → `withdrawn`; active task (or one finishing a cancellation); valid options. Effects: settles Claude/MCP/SDK/lost-host/lifecycle cards, start/post-merge/plan-review problems, `hold_for_plan_review`, and `NotifyAgent` to the current node **only if the group is complete** (`:4395-4409`).
  - `ReopenAnswer` (`:4095`): only a member of an answered group in an incomplete group.
  - `Withdraw` (`:4162-4212`): only the author (or System for holds, system cards and questions to the orchestrator); not answered or accepted; withdraws the whole group and deletes its answers; if it was the current final delivery → `invalidate` + `hand_back`.
  - `MarkRead` (`:4273`), `RequestClarification` (`:4213`, open, to the user, not hook or System), `SnoozeCard`/`UnsnoozeCard`/`ResurfaceDueCards` (`rules/snooze.rs`), `SettleSuggestion` (`:898-956`), `SettleStall`, `SettlePlanReview`.
  - Deliveries: `SubmitFeedback` (`:4945`), `AcceptDelivery` (`:4987`), `DecideProposedPlan` (`:4999`), `AuthorizeMerge` (`:5012`), see M-CARDS-2.
  - Sweeps: `withdraw_deliveries_asked_after` (new question next to a final delivery → returns the delivery), `withdraw_deliveries_behind_their_pr` (push after delivering → returns it), `retire_cards_of_ended_tasks` (unanswered questions of ended tasks, except System-authored ones; `EndedUnanswered` notice if it ended done), `reconcile_snoozed_cards`.
- **Rejected**: all with a typed `DomainError` (code + field + fix); no panic reachable by input (the `unwrap`s come after prior checks). A command that changes nothing → `no_effect`.
- **Terminals**: answered (except `ReopenAnswer` in groups), withdrawn, read, accepted-and-task-terminal. `acceptance.valid` is **not** terminal: `invalidate` (`:1646-1689`) turns it off on any feedback, document revision, push or reassignment.
- **Invariants**: enforced (`validate`): identity/revision, agent card with task, coherent subject, answers within the options, valid acceptance only on the current final delivery (`rules.rs:6190`), feedback directed at a target of the delivery. **Assumed, not enforced**: (a) `final_presentation` points to a non-withdrawn delivery (`validate` only requires it to be a final delivery of the task, `:5964-5976`) → F-CARDS-5; (b) a question group stays complete or is withdrawn whole → F-CARDS-2; (c) the targets of the current delivery stay current (documents) → F-CARDS-4; (d) the contract's "Goes away when" (`card_contract.rs:200-214`, column of `docs/architecture/cards.md`) is **documentation only**: `Contract.retires` is only read to generate the table (`card_contract.rs:515`); no rule applies it.
- **Persistence and restart**: everything persisted; the Claude hook waiting on a `HoldHook` lives in the host process (out of scope).

### M-CARDS-2 · Final delivery, acceptance and merge (`acceptance_valid` × `MergeAuthorization`)
- **Kind**: explicit, composite. **Defined in**: `Acceptance{valid}` (`model.rs:834-838`), `MergeAuthorization{valid, attempt}` with `MergeAttemptState{Unclaimed, Claimed, Uncertain, Confirmed, Failed}` (`model.rs:1390-1414`), `Feedback{instruction}`, `stack_readiness`, lifecycle journal (`Presented`/`HandedBack`/`Accepted`).
- **Source of truth**: `interactions.payload`, `merge_authorizations`, `pull_requests.review`, `stack_readiness`.
- **Transitions** (summary):

| From | Event [guard] | To / effect |
|---|---|---|
| no delivery | `PresentResult{is_final}` [node assigned; all open PRs included; no separate questions; checklist covered] | current; `hand_back` of the previous one; `invalidate` |
| current | `AcceptDelivery` [user; same targets; no plan; not blocked; every PR authorizable; no pending message] | accepted + authorizations `accept-<rev>-<pr>` |
| current (with plan) | `DecideProposedPlan{Implement}` [same; not accepted; request `implement-<id>` does not exist] | accepted + `Request implement-<id>` + `request_plans` + `NotifyOrchestrator` |
| current/accepted | `SubmitFeedback` [user; current targets; not (closing ∧ PR merged)] | `invalidate` (or keeps the user's merges if Reply/Question) + feedback + `NotifyAgent` |
| current | `AuthorizeMerge` [PR open at that head; method; no attempt in progress] | valid authorization (invalidates others of the PR) |
| authorization | `ClaimMergeAuthorization` [System; valid; Unclaimed; remote head = approved; base verified; stack ready] | Claimed |
| Claimed | `ObserveMergeAttempt{Uncertain\|Failed}` / `ObservePrState{Merged}` | Uncertain/Failed / Confirmed |
| current | push to the PR (new head) | sweep: `invalidate` + withdrawal + `hand_back` |
| current | PR closed and task discarded | `cancel` (`rules.rs:5155-5158`) |
| accepted and settled | node ends / `CompleteTask` | task Done |

- **Rejected**: `acceptance_stale` (targets changed or the delivery is no longer the current one), `stale_head`, `pending_user_reply`, `unanswered_message`, `blocked`, `delivery_uncertain`, `plan_choice_required`, `task_closing`.
- **Invariants**: assumed: approving a plan is irreversible in its effects (request created and orchestrator notified) but the acceptance can be invalidated afterwards → F-CARDS-3.

### M-CARDS-3 · Question group (`QuestionGroup`)
- **Kind**: implicit. **Defined in**: `card.group{id,index,total}` on each member; ids `<group>-q<n>` (`model.rs:1100-1129`, `rules.rs:204-221`). Complete = all non-withdrawn members answered (`group_complete`, `rules.rs:1069-1071`); `group_members` includes the withdrawn ones (`:1030-1037`).
- **Events**: `AskQuestions`/`HoldQuestions` (create N), `AnswerQuestion` (one), `ReopenAnswer` (undoes one while the group is incomplete), `Withdraw` (withdraws all and deletes answers). There is **no** group handling in `replaces` or in `slot_holders` → F-CARDS-2.
- **Effect on completion**: `NotifyAgent` with all the answers; `hold_for_plan_review` (`plan_review.rs:76-106`).

### M-CARDS-4 · Snooze (`snoozed_until`, `resurfaced_at_revision`)
- **Kind**: implicit. **Defined in**: `rules/snooze.rs:4-128`.
- **States and transitions**: visible → snoozed (`SnoozeCard`, guard: open, to the user, in the inbox, task not archived, `until > now`) → visible again by `ResurfaceDueCards{now}` (System), `UnsnoozeCard` (user), new activity of a node on the task, or replacement (`resurfaced_at_revision` on the new card, `rules.rs:808-821`). Cleared on withdrawal (`retire`) or on close; a task that ends → the snoozed card is withdrawn.
- **Clock**: `now` is supplied by the adapter; nothing in the domain compares against a clock. No findings.

### M-CARDS-5 · Clarification (`clarification`, `answer_clarification`)
- **Kind**: implicit. `RequestClarification` (`rules.rs:4213-4246`) sets `clarification`; R7 (`clarification_answered`, `:1086-1108`) forces the author's next non-Know card to replace it (`replaces`); the MCP tool `answer_clarification` fills `replaces` and forwards to `ask`, `request_action`, `request_input`, `request_test` or `present_result` (`cli-mcp/src/mcp.rs:1195-1205`). It is also offered on open final deliveries (`projection.rs:592-611`). A delivery with a clarification holds its merge (`merge_held`, `projection.rs:246-249`). Issue #317 proposes withdrawing it whole. It is the main entry to F-CARDS-5.

### M-CARDS-6 · Slots and replacement (`replaces`, `CardSlot`)
- **Kind**: explicit, small. **Defined in**: `CardSlot{Plan, FinalResult, Problem{cause}}` (`model.rs:1078-1098`). In a task the slot belongs to the task whatever the author; outside, to the author (`rules.rs:1149-1160`). `replaces` requires same author, not hook, not answered, open (`:1129-1147`).
- **Copies that diverge**: `replaces` rejects hook cards, `slot_holders` does not (H-CARDS-1); neither applies the effects of `Withdraw` on the current delivery (`invalidate` + `hand_back`) → F-CARDS-5.

### M-CARDS-7 · Plan review of dependents (`plan_reviews`)
- **Kind**: explicit. **Defined in**: `PlanReview{task_id, decisions, dependents: BTreeMap<TaskId, Option<PlanReviewSettlement>>}` (`model.rs:791-832`), `rules/plan_review.rs`.
- **Events**: opened when the user completes a decision (`Decide`, not permission/plan/elicitation) of a task with unstarted dependents: each dependent gets `HeldForPlanReview` and the orchestrator `ReviewDependentPlans`. Settled per dependent with `SettlePlanReview{Unchanged|PlanChanged|Cancelled}` (orchestrator; System only `Unchanged`, `Answer` deadline of M-ORCH-6) or in bulk with the problem card `keep_plans` (`KeptByUser`).
- **Guards and invariants**: `not_held_for_plan_review` in `AssignTask`; `validate_plan_reviews` (`plan_review.rs:212-227`): no open dependent with a node. A cancelled dependent stays `None` forever but `plan_review_waiting` filters it. No finding of its own.

### M-CARDS-8 · Checklist of the approved plan (`checklists`, `checklist_marks`, `post_merge_work`)
- **Kind**: implicit. **Defined in**: `checklists: TaskId → Checklist{plan, items}` (tables `task_checklists`, `delivery_checklists`). Origin: approving a plan held by a hook (`AnswerQuestion` with `approve`, `rules.rs:4371-4376`), creating the task of a `request_plans` (`rules.rs:3676-3684`), `plan_items` (`take_plan_items`, `:1838-1863`) or `continues` (`:3664-3672`). Marks per final delivery (`checklist_marks_cover_the_plan`, `:600-662`); proof required to complete, confirm cleanup and finish post-merge work (`require_checklist_proof`, `:668`). `DeclarePostMergeWork` lets a PR be delivered with partial marks (`docs/architecture/cards.md`, "A PR review before publication").
- **Invariants**: enforced (`validate`, `rules.rs:6226-6245`): each checklist belongs to a plan of its request; each item has a single task. Assumed: items moved with `plan_items` to a task that is later cancelled only come back through `continues` (no rule returns them). No confirmed finding.

### M-CARDS-9 · Documents and revisions
- **Kind**: implicit. **Defined in**: `Document{current_revision}`, immutable `DocumentRevision{number}`, `DocumentReview{decisions}` per `(task, document)` (`model.rs:488-515`).
- **Events**: `CreateDocument` (node), `ReviseDocument` (node; invalidates **all** tasks that link the document, `rules.rs:4502-4552`), `LinkTaskDocument` (manager; shares a document between tasks), `DecideDocumentReview` (user; current revision only). A final delivery with `DocumentRevision` is valid only while that revision is the current one (`targets`, `rules.rs:690-712`). No sweep returns the delivery when the revision changes → F-CARDS-4.

### M-CARDS-10 · Requests (`Request`)
- **Kind**: implicit. **Defined in**: `Request{closed, withdrawn, answer, message, maintenance, cleared, origin, folded_into, reply_to}` (`model.rs:41-88`) + the request's tasks + `request_plans` + cards `system:request_tasks`/`system:request_done`/`<id>-answer`.
- **States**: being defined (open without tasks, `is_defining`), with live tasks, with all tasks ended and open (only the orchestrator moves it), closed with answer (Done/answer card), closed withdrawn (no card), folded into another task, cleared (`cleared`).
- **Transitions**: `CreateRequest`/`CreateCardReply` (user), `TitleRequest`, `CloseRequest` (`rules.rs:3511`; `close_request` `:1342-1366`: Done card if it has tasks and the user asked for it, answer card if not), `close_delivered_plans` (automatic close only if all tasks are Done and marked every item of every plan, `:1368-1410`), `close_finished_messages` (messages, no card), `withdraw_cancelled_requests` (the user cancelled all unstarted tasks: closed withdrawn, no card), `AdoptOpenPr` (reopens the request, `rules.rs:5272-5277`), `ClearCompleted`. Effect `RequestTasksOver` (`:2961-2969`) **only once**, in the commit in which the last task ends.
- **Orchestrator inbox**: `orchestrator_inbox::steps` and `unresolved` only chase requests **without tasks** (`orchestrator_inbox.rs:61-71,132-142`); those with all their tasks ended depend on the single message → F-CARDS-1.

### M-CARDS-11 · Task creation, dependencies and waits
- **Kind**: implicit. **Defined in**: `Task.dependencies` (objective, "landed"), `delivered_dependencies` (reviewer waits for the delivery), `waiting_for_tasks` + `ended_task_waits` (wait declared by a node, ends with any final state), `stack` (`StackDependency`), `blocks` (reviewer). Acyclic graph (`dependency_graph`, `rules.rs:1865-1898`; `blocks_graph` `:1900-1934`). Derived state `WaitingDependency` (`lifecycle.rs:2613-2650` `state_within_phase`).
- **Commands**: `CreateTask` (`rules.rs:3584-3697`, orchestrator only, with `launch`; carries no priority: F-ORCH-8), `SetDependencies` (`:3771`), `SetRelations` (`:3790`, rejects cancelled tasks), `WaitForTasks` (`:3747`), `DeclarePostMergeWork{waits_for}` (`wait_targets`, `:1778-1820`, rejects cancelled), `SetPriority` (`:3824`).
- **Dependent of a cancelled task**: `cancelled_dependency` (`ownership.rs:320-326`) → `Owed::Replan` only if the task has not started and has no node (`ownership.rs:352-354`); the orchestrator gets a reminder at every rest (M-ORCH-7, F-ORCH-2). `waiting_for_tasks` also ends with a cancelled one (`task_wait_finished = terminal`, `rules.rs:2007-2009`) and the node gets `dependencies_done_notice` with `cancelled`. See F-CARDS-6 for the unguarded entry.
- **Cancellation** (`cancel`, `rules.rs:2191-2235`; `close_task`, `rules/open_pr.rs:29-52`): `invalidate`, withdraws closing/start problems, unanswered questions and cards held by the lifecycle; `Finished{Cancelling}`; `NotifyAgent`. It does not touch dependents (the orchestrator handles them).

### M-CARDS-12 · Stacks (`stack`, `stack_readiness`)
- **Kind**: implicit. **Defined in**: `StackDependency{upstream_task, presentation_id, pr_id, head}` + `StackReadiness{ready}` published by the upstream node (`PublishStackReadiness`, `rules.rs:5181-5211`); `stack_ready`/`stack_ready_for_merge` block start and merge. `invalidate` turns `ready` off. A cancelled upstream: `cancelled_dependency` treats it as a cancelled dependency. No finding of its own (the identity of moved PRs belongs to the stable pull request identity work).

### Findings

#### F-CARDS-1 · A request whose tasks ended depends on a single message to the orchestrator: if that batch does not close it, it stays open forever with no result card (issue #353)
**Class**: CONFIRMED (that nothing claims it again). Whether the batch fails to close it depends on the model or on a cut, which is probabilistic. · **Severity**: S2 · **Probability**: medium (one omission by the model is enough; issue #353 observed it) · **Overlap**: issue #353 (NEW evidence: exact paths showing why there is no second chance). Related to F-ORCH-1 and F-STORE-3 (ways of losing the single message). `git log` of `orchestrator_inbox.rs`: no later fix.
- **Invalid state/transition and broken invariant**: a user request open, with all its tasks ended, no Done/answer card and no owner. Breaks "Inbox is the channel" (every request ends with a result card) and the rule that every state has an owner.
- **Evidence**:
  1. `RequestTasksOver` is only emitted in the commit in which the request **becomes** one with all tasks ended: `rules.rs:2961-2969` filters with `!ended_before.contains(id)`. The test `request_over_tests.rs:61` (`…hands_its_open_request_to_the_orchestrator_once`) fixes that "once".
  2. The effect becomes a single outbox message to the orchestrator (`store-sqlite/src/command.rs:351-363`).
  3. Automatic close only in two cases: `close_delivered_plans` (`rules.rs:1368-1410`: all tasks Done and every item of every plan marked) and `close_finished_messages` (`:1309-1314`, only `message`, no card). A normal request, or one with a cancelled task, or without a plan, does not qualify.
  4. The orchestrator inbox only chases requests **without tasks**: `orchestrator_inbox.rs:61-63` (`if tasks.any(...) { continue }`) in `steps`, and `:132-136` in `unresolved` (what a batch leaves without a result). A request with ended tasks appears in neither, so there is no reminder and no problem card.
  5. There is no deadline or domain obligation for it: `obligation()` is per task, and its tasks are ended.
- **Minimal sequence**: 1) user request R with a task T; 2) T ends (Done, or cancelled after starting); 3) the batch that receives the message ends without `close_request` (the model omits it, its `close_request` is refused, or a restart cuts the batch: F-ORCH-1, or the uncertain message is discarded at 24 h: F-STORE-3); 4) nothing mentions it again.
- **Impact**: the user never sees how their request ended; the request counts as open in the views.
- **Deterministic test and ablation**: orchestrator, pure domain with `wiring::open`, like the `orchestrator_inbox.rs` tests: create R and T, end T, mark the orchestrator's message as delivered without closing R, call `steps(&state)` and `unresolved(...)`: today neither mentions R. Assert there is a step (reminder or close) for R. Ablation: remove the fix and see `steps` empty.
- **Expected fix and files**: treat "open request with all its tasks ended" as an orchestrator-inbox case like requests without tasks (reminder at every rest with a cap, then close by Alondra with the Done card of `tell_it_is_done`, which already knows how to summarize cancelled tasks and merged PRs). Files: `engine-orchestrator/src/orchestrator_inbox.rs` (`steps`, `unresolved`), maybe `rules.rs` (close by System).

#### F-CARDS-2 · Replacing one question of a group leaves the group incomplete forever: the node never receives the other answers
**Class**: CONFIRMED · **Severity**: S2 · **Probability**: low (the node must replace one specific member) · **Overlap**: none in the plans of related work or in open issues. #317 (remove clarifications) does not cover it.
- **Invalid state/transition and broken invariant**: a group with one withdrawn member and the rest answered; `group_complete` is false forever, so the joint answer the contract promises ("the node gets every answer at once when the last one is answered", `model.rs:1060-1062`) never arrives. `Withdraw` does handle the whole group (`rules.rs:4192-4202`); `replaces` does not.
- **Evidence**:
  1. `replaceable` (`rules.rs:1129-1147`) only looks at author, hook, answered and open: it accepts a group member.
  2. `insert_interaction` withdraws only that card (`rules.rs:817-821`), not its siblings, and does not delete their answers.
  3. R5 does not prevent it if the new card is not blocking or not "one at a time": `one_blocking_ask` exits at `:1111-1113` (`!blocking || !kind.one_at_a_time()`); `request_test` is `Test` and its `blocking` defaults to `false` (`cli-mcp/src/mcp.rs:1521`); `ask` allows `blocking: false`.
  4. `group_members` includes the withdrawn one (`rules.rs:1030-1037`) and `group_complete` requires all `!withdrawn && answered` (`:1069-1071`): false forever.
  5. `AnswerQuestion` only notifies the node if the group is complete (`:4395-4409`); `hold_for_plan_review` does not act either (`plan_review.rs:79-81`).
- **Minimal sequence**: 1) the node asks a group `g` (q1, q2, q3); 2) the node rephrases q1 with `ask{blocking:false, replaces:"g-q1"}` (or `request_test` with `replaces`); 3) the user answers q2, q3 and the new one; 4) the node receives only the answer of the new card; the answers of q2 and q3 never arrive (and a decision that was meant to hold dependents' plans does not hold them).
- **Impact**: user answers lost; the node stays without them or stops, and the stop is resolved with a default instruction.
- **Deterministic test and ablation**: pure domain, `card_tests.rs`, with `asks(three())`, `ask(...)`, `answer_card`, `told_the_node`: `asks(three())`; `ask("again","t",false, decide(..) with replaces=Some("g-q1"))`; answer `g-q2`, `g-q3`, `again`; assert the node receives the answers of q2 and q3 (or that the replacement is rejected). Today `told_the_node` counts 1 (only `again`) and no notice carries q2/q3. Ablation: with the fix in `replaceable`, revert it and see the test fail.
- **Expected fix and files**: either `replaceable` rejects a group member (`card.replaces` must name the whole group and replace it with another group), or replacing a member withdraws the group like `Withdraw`. Files: `rules.rs` (`replaceable`, `insert_interaction`).

#### F-CARDS-3 · Requesting changes (or just replying) on an already approved plan while its task closes disapproves it but leaves the implementation request alive: the plan is implemented twice or with the old version
**Class**: CONFIRMED · **Severity**: S2 · **Probability**: low-medium (window = how long the node takes to clean up; the action stays enabled on the card) · **Overlap**: none known. #326 ("explain it simply" in plan review) touches the same card but not this rule.
- **Invalid state/transition and broken invariant**: `request_plans[implement-<p>]` exists and the orchestrator is already building that plan, while delivery `p` is back to unaccepted and its node rewrites the plan. A second approval creates another independent implementation request.
- **Evidence**:
  1. `DecideProposedPlan` accepts the delivery and in the same commit creates `Request implement-<p>`, `request_plans` and `NotifyOrchestrator` (`rules.rs:4999-5011`).
  2. With a live node the task does not end: `finish_settled_deliveries` requires `!wraps_up(t)` (`lifecycle.rs:2570-2573`); `wrap_up_step` asks for cleanup (`rules.rs:2549-2570`). The task stays non-terminal.
  3. The inbox offers `submit_feedback` on every delivery, judged by its guard (`projection.rs:513-527`).
  4. `feedback_guard` (`guards.rs:102-123`) only rejects during closing if the delivery has a **merged** PR (`in_cleanup && merged_delivery`); a plan is a text delivery: `merged_delivery` is false (`rules.rs:2077-2086`). `presentation()` passes (task not terminal, still the current one, text targets).
  5. `SubmitFeedback` invalidates the acceptance: `Changes` with `invalidate_feedback` → `invalidate` (`rules.rs:4966-4970`, `:1646-1668`); `Reply`/`Question` with `invalidate_keeping_user_merges`, which only restores acceptances of deliveries with a user merge authorization (`:1690-1707`): a plan has none, so it is also disapproved. `wrap_up_step` clears `asked_at` (`:2563-2567`), the node leaves cleanup and receives "The user requested changes…".
  6. Nothing touches `request_plans` or the created request. The node presents a new plan `p2`; `decide_plan_guard` only checks that `implement-<p2>` does not exist (`guards.rs:242-244`) → second request; `CreateTask` on it passes `approved_plan_has_task` because it compares by `presentation_id` (`rules.rs:3676-3680`).
- **Minimal sequence**: 1) a node with plan `p` delivered; 2) the user approves (`DecideProposedPlan`); 3) before the node finishes cleanup, the user presses "request changes" or replies on the same card (another device with the view refreshed, or the same one); 4) the orchestrator has already created tasks for `implement-p`; 5) the node delivers `p2`; 6) the user approves `p2` → two implementation requests.
- **Impact**: duplicated work or work built on a plan the user wanted to change; the changes do not reach the orchestrator.
- **Deterministic test and ablation**: pure domain, `proposed_plan_tests.rs` (`delivered_with_plan()` already has a live node): `decide(Implement)`; then `SubmitFeedback{Changes}` on `report` with `targets: text()`. Today it passes and `accepted(&s,"report")` is false with `s.request_plans["implement-report"]` present. Assert the chosen behavior (rejection `plan_approved`, or the request withdrawn/the orchestrator notified). Ablation: revert the new guard and see the test let the feedback through.
- **Expected fix and files**: a product decision. Suggested direction: an approved plan admits no change-feedback; its card only offers replying without disapproving (or the feedback opens a new linked request). Files: `guards.rs` (`feedback_guard`), `rules.rs` (`invalidate_keeping_user_merges` for plans), `projection.rs` (offered action).

#### F-CARDS-4 · A new revision of a document leaves dead the final delivery that carries the previous one: neither accepting nor requesting changes works and nobody returns it
**Class**: CONFIRMED · **Severity**: S2 · **Probability**: low (documents shared or revised after delivering) · **Overlap**: none known.
- **Invalid state/transition and broken invariant**: a `ForYourReview` task with a current delivery whose buttons all fail with `acceptance_stale`; the node is not notified (if the document was revised by another task). Breaks rule 7 in reverse (a card that waits for the user and that the user cannot resolve) and "a state with an owner".
- **Evidence**:
  1. `ReviseDocument` changes `current_revision` and `invalidate`s **all** tasks that link the document (`rules.rs:4543-4549`), without a `NotifyAgent` effect and without returning any delivery.
  2. `presentation()` calls `targets()` (`rules.rs:595`), which rejects a `DocumentRevision` that is no longer the current one (`:710-712`, `acceptance_stale`). Used by `accept_delivery_guard` (`guards.rs:150`), `decide_plan_guard`, `feedback_guard` (`guards.rs:120`) and `CompleteTask`.
  3. For PRs the equivalent sweep exists (`withdraw_deliveries_behind_their_pr`, `rules.rs:2395-2426`: new head → withdraw + `hand_back` + notice to the node); for documents there is none.
  4. The inbox still lists the delivery: `card_waits_for_user` does not look at whether the targets are current (`projection.rs:393-401`), and `attention` stays `Act` because `open(p)` does not look at them (`rules.rs:1006-1021`).
  5. Sharing a document between tasks is allowed to the manager (`LinkTaskDocument`, `rules.rs:4472-4501`; MCP tool `link_task_document`, `cli-mcp/src/mcp.rs:630`).
  6. The test `tests.rs:1504-1527` only checks that `AcceptDelivery` fails and the task does not end up Done; it does not look at the inbox or at the delivery's fate.
- **Minimal sequence**: 1) tasks A and B link document D (v1); 2) B delivers final with `DocumentRevision{D, v1}`; 3) A's node runs `revise_document(D)` → v2; 4) the user opens B's card: "Accept" and "Request changes" return `acceptance_stale`; B's node knows nothing. Single-task variant: B's node revises D after delivering and ends its turn without presenting again.
- **Impact**: a task stuck in "for review" with a card that has no way out; only cancellation or a message to the node by another route gets it out.
- **Deterministic test and ablation**: pure domain, extending `tests.rs:1504`: after `ReviseDocument`, assert the task is back in progress (`final_presentation == None`, `HandedBack` in its journal) and there is a `NotifyAgent` to B's node; with two tasks, that B's node receives the notice. Today `final_presentation` still points to the delivery and `SubmitFeedback` also fails with `acceptance_stale`. Ablation: remove the new sweep and see the test fail.
- **Expected fix and files**: a sweep `withdraw_deliveries_behind_their_document` symmetric to the PR one (withdraw, `invalidate`, `hand_back` with a notice "the document changed: present the current revision") for each affected task. Files: `rules.rs` (`prepare_transition`, new sweep next to `:2954-2955`).

#### F-CARDS-5 · Replacing the current final delivery with a card that is not another final delivery withdraws it without returning it: the task stays "Delivered" with no card for the user
**Class**: CONFIRMED · **Severity**: S2 · **Probability**: low-medium (answering a clarification with a note is natural; #317 would remove the main entry but not the orchestrator one) · **Overlap**: #317 (remove clarifications) removes entry 5 but not 4/5-orchestrator; NEW evidence. #376 ("Action cards require an explicit response to the correct card") is a different matter.
- **Invalid state/transition and broken invariant**: `task.final_presentation` points to a `withdrawn` delivery; the journal is still `Presented` (phase Delivered, obligation `User`); the inbox lists nothing of the task. Breaks rule 7 ("A task waits for the user only on a card the inbox lists") and the assumed invariant "the current delivery is not withdrawn" (`validate`, `rules.rs:5964-5976`, does not check it).
- **Evidence**:
  1. `Withdraw` of the current delivery does `invalidate` + `hand_back` (`rules.rs:4183-4204`). `replaces` does not: `insert_interaction` only calls `retire` (`:817-821`, `retire` at `:1022-1028`).
  2. `replaceable` accepts as the old one any open card of the same author, including its final delivery (`:1129-1147`).
  3. The sweeps do not fix it: `withdraw_deliveries_asked_after` only reacts to a question **to the user** (`asked_aside` → `unanswered_card` requires `recipient == User` and a question payload, `:2301-2313`); `withdraw_deliveries_behind_their_pr` skips withdrawn deliveries (`:2402`).
  4. `PresentResult{is_final:false}` with `card.replaces` reaches `insert_interaction` (`:4879-4898`) and does not `hand_back` because it is not final (`:4908`); it is only rejected if there is an open PR (`pr_needs_final`, `:4848-4851`).
  5. The natural entry is clarification: the user asks to clarify a final delivery (offered on open deliveries, `projection.rs:592-611`); R7 forces the node's next non-Know card to replace it (`rules.rs:1086-1108`); `answer_clarification` accepts `present_result` as the replacement and sets `replaces` (`cli-mcp/src/mcp.rs:1195-1205`). Another: `ask`/`request_action` addressed to the orchestrator with `replaces` = the delivery.
  6. Result: `state_within_phase` → `ForYourReview` (`lifecycle.rs:2625-2647`, `attention` = `Idle` because `!open(p)`, `attention.rs:171`); `card_waits_for_user` lists neither the delivery (attention is not `Act`) nor the progress (not the current one) (`projection.rs:393-401`).
  7. Side effect: `presentation()` does not look at `withdrawn` (`rules.rs:577-597`), so a client with the old button can accept the withdrawn delivery and the task closes with it.
- **Minimal sequence**: 1) the node delivers a final text delivery `d1`; 2) the user presses "Ask for clarification" on `d1`; 3) the node answers with `answer_clarification{replacement:{tool:"present_result", arguments:{is_final:false,…}}}` (a progress note that clarifies); 4) `d1` is withdrawn, the task stays "Delivered", the inbox shows nothing and the node ends its turn.
- **Impact**: a task stopped at "For you" with no card; it only leaves if the node presents again or the user cancels/writes by another route.
- **Deterministic test and ablation**: pure domain, `card_tests.rs`: deliver final text `d1`; user `RequestClarification`; node `PresentResult{is_final:false, card.replaces: Some("d1")}`. Assert the task no longer reads `ForYourReview` with `final_presentation == Some(d1)` (there must be `HandedBack` or the command must be rejected) and that `situation` is not "Delivered" without a card in `inbox_items(&s)`. Today it fails. Variant with `AskQuestion` to the orchestrator. Ablation: revert the fix.
- **Expected fix and files**: replacing the current delivery with any card goes through the same path as `Withdraw` (`invalidate` + `hand_back`), or `replaceable` only allows replacing a final delivery with another final delivery. In addition `presentation()` must reject a withdrawn delivery. Files: `rules.rs` (`insert_interaction`, `replaceable`, `presentation`).

#### F-CARDS-6 · `CreateTask` and `SetDependencies` accept an already cancelled dependency: the task is born waiting for something that will not come (issue #323)
**Class**: CONFIRMED · **Severity**: S3 · **Probability**: medium (#350 describes it) · **Overlap**: #323 and #350 (NEW evidence: the unguarded entry and the asymmetry with `SetRelations`/`wait_targets`); F-ORCH-2 (the loop it feeds). The note in `task-states.md` ("Waiting for a cancelled task advances only when the orchestrator is idle…") describes the same gap.
- **Invalid state/transition and broken invariant**: a new task in `WaitingDependency` on a cancelled task; it only leaves if the orchestrator re-plans it, and that reminder is the loop of F-ORCH-2.
- **Evidence**:
  1. `CreateTask` copies `dependencies` without looking at their state; `dependency_graph` only checks existence and cycles (`rules.rs:3584-3663`, `:1865-1898`). `SetDependencies` likewise (`:3771-3789`).
  2. By contrast `SetRelations` rejects cancelled tasks (`:3794-3798`) and so does `wait_targets` (`:1810-1813`): the rule exists but not in the two most used commands.
  3. `landed` is never true for a cancelled one (`:1944-1948`) → `waits_are_over` fails → `WaitingDependency` (`lifecycle.rs:2622-2624`); `cancelled_dependency` → `Owed::Replan` only if the task has no node and has not started (`ownership.rs:320-326,352-354`). With an assigned node (`CreateTask{agent_id}`), the situation says "waiting for another task" and the node never gets a notice (`resume_waiting_nodes` only notifies when the wait ends, `rules.rs:1746-1776`).
  4. It is exactly the sequence of "moving" a task with `create_task` + `cancel_task` (#350) when the orchestrator creates the replacement pointing at the old one or does not re-point the dependents: there is no replacement command that re-points.
- **Minimal sequence**: 1) A cancelled; 2) the orchestrator creates B with `dependencies: {A}`; 3) B waits; 4) every orchestrator rest generates a reminder (F-ORCH-2) until it calls `set_dependencies`/`cancel_task`.
- **Impact**: a task that does not start and orchestrator batches wasted.
- **Deterministic test and ablation**: pure domain, `tests.rs`: cancel A; `CreateTask{dependencies:{A}}` and `SetDependencies{A}` must be rejected (`invalid_input`, field `dependencies`) like `SetRelations`. Today they pass. Ablation: remove the guard.
- **Expected fix and files**: the same guard as `SetRelations` in `CreateTask` and `SetDependencies`; and for #323, a replacement command that re-points the dependents of the cancelled task to its replacement in the same commit. Files: `rules.rs`.

### Hypotheses

#### H-CARDS-1 · The plan slot withdraws a card held by a hook
`replaceable` refuses to replace a hook card (`rules.rs:1136-1138`), but `slot_holders` (`:1149-1160`) withdraws any open card of the task's `Plan` slot, whatever the author and even if it is in `holds`. A held plan (`HoldHook{Plan}` with `Plan` content, `daemon-engine/src/domain_holds.rs:223`) would be withdrawn without an answer if another card with `Plan` content reaches the same task. Missing: who can create today a non-held card with `Plan` content (MCP passes the content through `unplan_content`, `cli-mcp/src/mcp.rs:1481`; the orchestrator or the HTTP route remain) and what the Claude hook does when its card appears withdrawn (does it deny or wait forever?).

#### H-CARDS-2 · Alondra problems that outlive their task and reappear when it is reopened
`retire_cards_of_ended_tasks` only withdraws questions whose author is not System (`unanswered_card`, `rules.rs:2301-2313`) and `finish_task` withdraws nothing (`lifecycle.rs:2538-2563`); `cancel` withdraws only closing/start problems and the journal's cards (`rules.rs:2196-2213`). An open `system:*_problem` on a Done task is still `open()` (hidden because the inbox requires an active task, `projection.rs:376-378`) and would come back to the inbox with `ReopenTask` (`rules.rs:5296-5316`, which does not touch it). The contract says `task_ended` for all of them, but `Contract.retires` is not applied (`card_contract.rs:515`). Missing: which system problems can still be open when the task reaches Done and whether any daemon sweep withdraws them ("resolved").

#### H-CARDS-3 · Changes requested during an already claimed merge are lost when the merge lands
`SubmitFeedback{Changes}` invalidates all authorizations even if one is `Claimed` (`rules.rs:1669-1676`) and `feedback_guard` does not look at attempts in progress; if GitHub completes the merge, `ObservePrState{Merged}` confirms it anyway (`rules.rs:5141-5154`) and the delivery is settled (`settled` only waits for `Reply` answers, `rules.rs:2356-2361`), so the task ends with the user's changes undone and the node receives both "changes" and "PR merged". Missing: check in `engine-github` whether the merge executor rereads `valid` after claiming and whether the task really ends with that feedback pending.

#### H-CARDS-4 · Retrying a non-blocking `ask` with another `interaction_id` duplicates the card
R8 only deduplicates notices (`repeated_notice`, `rules.rs:1073-1084`) and R5 only blocking questions; a blind retry after losing the reply creates two equal cards. The MCP instructions ask to inspect before repeating, so it depends on the model. S3.

#### H-CARDS-5 · Second Done card after reopening a request
`AdoptOpenPr` reopens the request and deletes its answer (`rules.rs:5276-5281`) but leaves its Done card; when it closes again, `write_system_card` appends `-` to the id and writes another (`rules.rs:1575-1577`). Cosmetic; missing: how the apps display it.

## Engine: admission, host ownership, machine watch and transports (`ENGINE`)

Scope: the engine that drives nodes, audited at commit `1567967b9`. Read end to end:
`crates/engine-admission/src/admission.rs`, `pauses.rs`, `engine-ports/src/admission.rs`;
`crates/engine-sdk/src/sdk_owner.rs` (take/adopt, `Owner`, `Owners`, `retake`/`take_again`, `take_all`,
`launch_admitted`, `start_bound`), `account_move.rs`, `provider_transition.rs` (phases and `advance`),
`ended_hosts.rs`, parts of `host_folders.rs`, `task_closer.rs` (`close_sdk_node`), `bg_handover.rs`
(`admit`/`finish`), `auto_launch.rs` (admission step); `crates/daemon-engine/src/obligation_watch.rs`,
`usage_pause.rs` (parts), `node_close.rs`, `gate.rs`, `wiring.rs` (`AdmissionEngine`), `start_failures.rs`.
Second pass: `machine_watch.rs`, `disk_peaks.rs`, `launches.rs`, `bg_sessions.rs` (M-ENGINE-11..17), and
`engine-live`, `engine-kernel/domain_runtime*`, `domain_transport.rs` (M-ENGINE-18..27). H-ENGINE-1 was
confirmed and is now F-ENGINE-12. The overlap of each finding was checked against the related-coverage notes
and the open-issues list.

Paths that are about to change (the Bazel layers work (phase 4d-runtime) / 5f): `DomainRuntime::of`
(`crates/engine-kernel/src/domain_runtime.rs`, used by `daemon-engine/src/wiring.rs`),
`crates/engine-sdk/src/sdk_owner/worker.rs`, `crates/engine-kernel/src/sdk_backend.rs`. No finding below depends on
them; the proposed tests use `Admission`, `Owners` and the test doubles, which do not move.

### M-ENGINE-1 · Admission gate (composite, implicit)
- **Kind**: composite, implicit. **Defined in**: `crates/engine-admission/src/admission.rs:139-169` (`Inner`),
  `:114-122` (`Saved`), `engine-ports/src/admission.rs:113-130` (`Pauses`).
- **Source of truth, split over four places**:
  - in memory: `admitted: Vec<(Key, Millis)>`, `asking`, `at_work`, `turns`, `taken`, `refused`, `told`;
  - `admission.json`: `last_admission`, `pauses.{owed, held, resumed}`, `turn_waits`;
  - the domain (SQLite): node pauses (`DomainState::paused_nodes`), which `lock_with` rereads on every lock
    (`:285-301`). `pauses.paused` is `skip_serializing`, so it is only read from old files;
  - the engine's SQLite: `launch_queue`, `auto_launches`, `start_retries`.
- **Copies and derivatives**: `Snapshot` (`:1129-1220`), `LivePacing` (`:560-649`) and the slot count of the
  `ReadIndex` (`working_tasks`, `:1008-1022`).
- **States of a queue key** (`Key::{Task, Run, Resume, Turn}`):

| State | Encoding |
|---|---|
| Out | no entry in `asking`/queue/`turn_waits` |
| Waiting (start) | `asking[key]`, or a `launch_queue` row, or a pause that `returns()` |
| Waiting (turn) | `waits_for_turn` in the domain + `turn_waits[task]` |
| Admitted (start/return) | `admitted` contains `(key, at)` and `now-at < IN_FLIGHT` |
| Turn granted | `turns ∋ task` |
| Turn taken | `taken[task] ⊇ slot_waiters` |
| At work | `at_work ∋ key` (return), or the task is in `working_tasks` |

- **Events**: `ask`, `may_take_turn`, `pass_at` (every `TICK` = 10 s, on `poke`, or on any domain change),
  `hold_back`, `pause_by_user`, `resume_now`, `hold_queued`/`release_queued`, `owner_ended`, `not_started`,
  `forget`.
- **Guards**: `hold_of`, then disk, then memory (`MacState::room`), then slots, then spacing (`:658-668`,
  `:80-95`). Turns only need a slot (`turns_to_grant`, `:51-72`).
- **Essential transitions**:

| From | Event [guard] | To | Effect |
|---|---|---|---|
| Waiting | `ask`/`pass` [head of queue, no hold] | Admitted | `last_admission`, disk `reserve_admission`; for `Resume`, `resume_locked` (`:840-852`) |
| Admitted | [`now-at ≥ IN_FLIGHT`] | gone, by clock | (`:847`, `:1140`) |
| Turn waiting | `grant_turns` | Granted | (`:884-897`) |
| Granted | `take_granted` | Taken | (`:863-868`) |
| Taken | [domain stops waiting] | out | `settle_turns` (`:919-928`) |
| Return admitted | `carries_slot_over` [≤ `IN_FLIGHT`] | Taken | (`:857-861`) |

- **Rejected**: returned as a typed `Err(Hold)`. A return that the domain rejects stays in `refused` with the
  revision of its lifecycle (silently, only a log).
- **Invariants**:
  - enforced: the check and the reservation happen under the gate mutex, on a state read under it (`ask`,
    `pass_at`);
  - assumed: "one slot per task", "a reservation has no timer" (doc) and "the age lives in `admission.json`".
    The last two do not hold (F-ENGINE-1, F-ENGINE-4).
- **Persistence and restart**:
  - lost: `admitted`, `turns`, `taken`, `asking`, `at_work`, `refused`;
  - survive: `turn_waits`, `held`, `resumed`, `owed`;
  - pauses are rebuilt from the domain.
- **Concurrency**:
  - one `RuntimeMutex<Inner>` protects the gate;
  - the `run` loop is a tokio task and **waits inside `pass_at` for the whole launch** (`:1395`), so while it
    launches there is no `grant_turns`, `to_halt` or `take_working_unowned`;
  - `may_take_turn` is called by the owner threads; `ask` is called by `auto_launch`, `domain_schedule_runner`,
    `provider_transition::admitted` and `launch_from`.

### M-ENGINE-2 · Node pause, gate side (explicit in the domain, mirrored in the gate)
- **Kind**: explicit in the domain, with a mirror in the gate. **Defined in**: `Reason`
  (`engine-ports/src/admission.rs:48-67`), `Paused::returns` (`:101-109`), `Cleanup` (`:69-76`) and `pauses.rs`.
- **Source of truth**: the domain (`NodeEvent::Paused/Resumed/PauseCleanup/PauseReleased`,
  `domain-rules/src/node_machine.rs:340-352, 509-534`). The gate translates with `hand_pauses`
  (`admission.rs:744-797`).
- **States**:
  - `Memory` (legacy only), `Declared`, `Usage` (`--bg` nodes only), `ByUser`, `ApiError`, `Disk`;
  - cleanup `Pending → Terminated → Done`, which only exists on `--bg` nodes;
  - extras in the gate: `released`, `resumed[session]` and `owed[session]`.
- **Transitions**:

| From | Event [guard] | To | Effect |
|---|---|---|---|
| paused | `returns()` / `resumed` | queued as `Key::Resume` | |
| queued | admitted | running | `resume_locked` removes the pause; `hand_pauses` emits `Resumed` |

- **Divergences observed**:
  - on SDK nodes, `hand_pauses` does not forward `PauseCleanup` or `PauseReleased` to the domain (`:752`), so
    `release_declared` (`:418-426`) has no effect. Harmless today because `returns()` ignores `released`;
  - the `resumed` entry is deleted in `resume_locked` (`:807-809`) **before** it is known whether the domain
    accepts `Resumed` (see H-ENGINE-3).
- **Timers**: `TERMINATE_AFTER` = 10 s and `KILL_AFTER` = 20 s (`pauses.rs:20-21`, `--bg` only).

### M-ENGINE-3 · Queued start paused by the user (implicit, across two stores)
- **Kind**: implicit. **Defined in**: `hold_queued` (`admission.rs:1224-1256`), `release_queued` (`:1267-1278`)
  and `Pauses.held`.
- **States**:
  - `Queued`: a `launch_queue` row in SQLite, or `asking` with a plan;
  - `Held`: `held[task]` in `admission.json`, no row;
  - `Back`: the row again.
- **Source of truth**: two stores with no common transaction (F-ENGINE-3).
- **Terminals**: none. Nothing deletes `held` when the task ends.

### M-ENGINE-4 · SDK host ownership per task (composite, implicit)
- **Kind**: composite, implicit. **Defined in**: `Owners` (`sdk_owner.rs:2761-2797`), `Driver` (`:221-292`),
  `Owner` (`:2501-2758`) and the owner generation in the store (`store-sqlite/src/owner.rs:742-757`;
  `reserve_owner` does a compare-and-swap). Also the folder's `host.lock` and the `stopped-by-alondra` mark.
- **States per task**:

| State | Encoding |
|---|---|
| No owner | no unfinished `Owner` for the agent, `in_hand` = 0, no live `retakes`, no move/transition holding it |
| In hand (launching/taking) | `in_hand[task] > 0` (a counter, **not exclusive**) |
| Taking (exclusive) | `taking_over ∋ task`: only `take_over_unowned` and `take_again` use it (`:137-152`) |
| With owner | `Owner` in `held`, thread alive, `ended=false` |
| Released (idle_release) | `lifecycle.host_released()` and the `Owner` finished |
| Waiting for space | `waiting_for_space ∋ task` |
| Lost | host-lost card in the domain + `host_losses_told` |

- **Paths that create or adopt a host**:
  - `start_bound` / `launch_prepared` (`:481`, `:730`);
  - `launch_admitted` (`:774`);
  - `take_host_into` (`:866`), through `adopt`, `end_unowned_host`, `end_and_claim_unowned_host` and
    `adopt_for_transition`;
  - `take_all` (`:1545`) and `adopt_all`;
  - the `Owner` itself with `replace_ended_host` (`:2604`);
  - `account_move::admit/finish`, `bg_handover::admit/finish` and `provider_transition::{start_target, advance}`.
- **Exclusion guards**:
  - `unowned()` (`:1520-1529`), which only the retake paths consult (`to_take`, `retake_waiting_for_space`,
    `wake_released`);
  - `TakeoverTurn`, in only two paths;
  - the store's CAS generation, which is the only universal barrier.
- **Copies and derivatives**: `add_sdk_owner` (`:2925-2934`) keeps **every** unfinished `Owner` of the same agent.
  It does not prevent duplicates.
- **Persistence and restart**:
  - `adopt_all` ends the hosts of finished tasks and takes the rest in parallel;
  - the drivers hold `in_hand` (`Driver::new`, `:1703-1706`) until `add_sdk_owner`, so the window between
    `take_all` and `add_sdk_owner` is covered.

### M-ENGINE-5 · `Owner` thread loop (explicit in code)
- **Kind**: explicit. **Defined in**: `sdk_owner.rs:2535-2668`.
- **States**: `Stepping`, `Releasing` (`release`/`releases`), `Replacing` (`replacement`, `relaunches`,
  `silent_hosts` with `RELAUNCH_WAITS` waits of 2 s to 300 s), `Lost` (with `lost()`), `Stopped`/`Crashed`.
- **Terminals**: `ended=true` or the thread finished. Nothing reopens them: a new `Owner` is another instance.
- **Deadlines outside the policy**:
  - `opening_deadline` = `io_timeout_seconds` (120 s by default, `sdk_backend.rs:145`): after that time a host
    that did not open its session is replaced (`:1967-1976`, `:2219`);
  - `RELAUNCH_WAITS` and `SILENT_HOSTS` = 3, which ends in `lost()`.

### M-ENGINE-6 · Account move (explicit, disabled)
- **Kind**: explicit. **Defined in**: `AccountMoveStep` `Planned → HostStopped → Admitted → Done | Undone`
  (`account_move.rs:189-212`).
- **Current state**: `enabled()` always returns `false` (`:34-36`) and `refused()` always returns `Some`
  (`:116-118`). Only historical journals are reconciled: `Planned`/`HostStopped` go to `undo` and `Admitted`
  goes to `finish`.
- **Observation**: a historical move in `Admitted` whose destination never showed `ResourceAvailable` makes
  `finish` fail in `require_started_destination` (`:121-142`, `:241-244`). That calls `lost()` on every `tick`:
  every 10 s and on every domain change, forever (`:290-298`). The move never reaches `Done` or `Undone`. The
  user only has the card to cancel it. Not counted as a finding because the result is as designed; it is noise
  and an unreachable terminal.

### M-ENGINE-7 · Provider/model transition (explicit)
- **Kind**: explicit. **Defined in**: `Phase` validated in the store
  (`store-sqlite/src/provider_transition.rs:50-60`) and `advance` (`provider_transition.rs:418-505`).
- **States**: `Requested → Draining → SourceFenced → ContextReady → TargetAdmitted → TargetReady → Consumed →
  Done`. Also `Aborted`, `StartRefused` and `Uncertain`.
- **Source of truth**: a SQLite table. The fences (`EndedHost` with its lock held) live in memory
  (`Transitions.fences`) and are rebuilt with `ensure_fence`.
- **Rules**: any `Err` from `advance` leads to `abort()` in `pass` (`:521-526`). `Kind::Model` in transfer phases
  is always rejected: transfer is disabled.
- **Concurrency**: the `advancing` mutex serializes its own steps. It does **not** exclude retakes that were
  already running (F-ENGINE-12).

### M-ENGINE-8 · Obligation clock (implicit, in memory)
- **Kind**: implicit. **Defined in**: `obligation_watch.rs:46-56` (`Seen`), with `step`, `observe` and `enforce`.
- **States per task**: `(party, owed, since, first_seen, clock, reported)`. The policy lives in
  `domain-rules/src/obligation_policy.rs`.
- **Persistence and restart**: everything is lost. Deadlines count again from the first observation, which is
  safe: it only delays.
- `effective()` (`:96-101`) turns `HandInput`/`HandDecision` into `Room(Gate)` when the gate says
  `waits_for_slot`.

### M-ENGINE-9 · Global usage pause (implicit, in memory, legacy)
- **Kind**: implicit. **Defined in**: `usage_pause.rs:41-53` (`Record`).
- **States**: `exhausted`, `announced`, `resumed_at`, `strikes`, `by_margin` and `settled_cards`.
- **Persistence and restart**: rebuilt with `usage_probe`. The gate's `Reason::Usage` pauses are only created for
  `--bg` nodes (`:428-445`); SDK nodes wait on `ClaudeHeld` cards of the domain, which belong to another audit.

### M-ENGINE-10 · Closing a task's SDK node (implicit)
- **Kind**: implicit. **Defined in**: the `TaskClosing` row (`attempts`, `next_attempt`, `rm_started_at`,
  `attempted_cleaned_at`, `since`) plus `due()` (`task_closer.rs:113-123`) and `close_sdk_node` (`:458-530`).
- **Deadlines outside the policy**: `Timing.wait` (cleanup wait), `RETRY` = 5 min and `MAX_FAILURES` = 3, which
  ends in a card.

### Timers that act on state outside `obligation_policy`

`task-states.md:66-67` says that policy is "the only place a deadline may exist".

| Constant | Where | What it triggers |
|---|---|---|
| `IN_FLIGHT` 3 min | `admission.rs:43` | Frees the slot of an admitted start or return (F-ENGINE-1) |
| `opening_deadline` = `io_timeout_seconds` 120 s | `sdk_owner.rs:1859, 1967` | Replaces a host that did not open its session (overlaps the stalled-node recovery work) |
| `RELAUNCH_WAITS`, `SILENT_HOSTS` | `sdk_owner.rs:52-55` | Host retries and `lost()` |
| `UNOWNED_TAKE_EVERY` 60 s | `sdk_owner.rs:1274` | Cadence of retaking tasks without an owner |
| idle_release 2 min | `idle_release.rs` | Closes the host of an idle node (documented in admission.md) |
| `UNKNOWN_RESET_WAIT_MS` 30 min | `usage_pause.rs:31,485` | An exhausted window with no reset time stops blocking |
| `TERMINATE_AFTER`/`KILL_AFTER` | `pauses.rs:20-21` | Signals to processes of paused `--bg` nodes |
| `Timing.wait`, `RETRY` | `task_closer.rs:30-55` | Forced close after the cleanup wait |

Second-pass coverage A (M-ENGINE-11..17): `crates/engine-admission/src/machine_watch.rs` (code, 1-1068, and the tests
that fix the design), `crates/engine-admission/src/disk_peaks.rs`, `crates/engine-kernel/src/launches.rs`,
`crates/engine-kernel/src/bg_sessions.rs`. Followed only as far as needed to check guards:
`engine-admission/src/admission.rs` (`admit`, `snapshot_locked`, `working_tasks`, `reserving_tasks`,
`hold_for_disk`), `engine-ports/src/disk.rs`, `system/src/disk.rs`, `daemon-engine-machine/src/{machine,swap_disk}.rs`,
`engine-sdk/src/bg_handover.rs`, `store-sqlite/src/handover.rs`, `engine-ports/src/journal/handover.rs`,
`engine-sdk/src/sdk_owner.rs` (`start_bound`), `domain-rules/src/rules.rs` (`AdmitExecution`, `RecordTaskWorktree`),
`engine-kernel/src/domain_runtime.rs` (`set_task_worktree`, `changed`), `engine-worktrees/src/cancelled_worktrees.rs`,
`daemon-server/src/domain_bootstrap.rs` (`agent_waiting_machine`), `alondrad/src/{lib,assemble}.rs`. Not audited:
`machine_state.rs` (`MacState`, memory and swap) beyond its use. The `git log` of the four main files shows only crate
and port moves (#298, #300, #363, #366, #406, #434, #445, #447, #478, #496), with no earlier fix for what follows.

### M-ENGINE-11 · Machine incident (explicit: `Option<Incident>` + `step_with_disk_measurement`)
- **Kind**: explicit. **Defined in**: `Incident` (`machine_watch.rs:45-53`), `step_with_disk_measurement`
  (`:279-316`), `open` (`:318-323`), `failing` (`:221-223`), orchestrated by `look` (`:987-1039`) and the `watch`
  loop (`:1050-1066`).
- **Source of truth**: `Record.incident` and `Record.incident_volume` in memory, saved to `machine-incident.json`
  (`FILE`, `:29`) with an atomic write (`save`, `:399-404`).
- **Copies and derivatives**:
  - `disk_low()` (`:481-483`): read by `refuse_launch_on` (`:818-826`) and by `domain_schedule_runner` (`:398`,
    `:636`);
  - `carded` in `disk_room_for` (`:743-748`): with a disk incident open, the gate holds every start and return
    (`Hold::Disk`);
  - `has_space` (`:1041-1044`) triggers `retake_waiting_for_space`.
- **States**:

| State | Encoding |
|---|---|
| No incident | `incident = None` |
| Disk open, not recovered | `Some{cause: Disk, healthy: 0}` |
| Disk open, one healthy sample | `Some{cause: Disk, healthy: 1}` |
| `Declared` (legacy) | `Some{cause: Declared}`: only from old files; closed on the first look |
| `Memory` (legacy) | discarded on load (`persisted_in`, `:366-373`) |

  Product with the volume: `incident_volume = Some(mount)` when it was opened by a volume that is not the Mac's
  disk.
- **Events and guards**:
  - `look(sample)` with `measured_disk = valid_disk(boot) && measured_volumes` (`:992-996`), where
    `measured_volumes` requires `volume_readings_fresh` if any volume is watched or learned;
  - `fixed` (the "I fixed it" answer on the card, `:919-926`): **dead**. `Action::Log` only writes to the log
    (`:931-934`) and no card ever exists, so `fixed` is always false and `release_declared` is never called from
    here;
  - `suspend_unmounted` (`:510-529`): with a complete mount table lacking the incident's volume, it withdraws it.
- **Transitions**:

| From | Event [guard] | To / effect |
|---|---|---|
| None | sample with low disk (`failing`), even from cache | Open `healthy=0`; `Action::Log` |
| Open | unmeasured sample (`!measured_disk`) | Open `healthy=0` (`:281-284`) |
| Open | healthy measured sample | `healthy+1`; at 2, None + `Withdraw` + `Release` (`:291-299`) |
| Open | low measured sample | `healthy=0`; new card only on the first stop or with `fixed` |
| Open (volume X) | X absent from a complete table | None + `Withdraw` + `Release` (`:1021-1026`) |
| `Declared` | any look | None (`declared_only`, `:292-293`) |

- **Rejected**: nothing typed. Invalid samples are ignored: they do not open on their own and they reset `healthy`.
- **Terminals**: none. None reopens with the next low sample.
- **Invariants**:
  - enforced: one incident at a time; a disk incident only closes with two consecutive healthy measured samples;
  - assumed: "one sample = one measurement". It does not hold, see F-ENGINE-6.
- **Persistence and restart**:
  - survive: `incident` (with `healthy`), `incident_volume`, `declared`, `volumes`, `build_volumes`, `peaks`,
    `stretches` and `sampled_at`;
  - `volume_mounts` goes back to `None`, so until the first mount table no absence can be confirmed.
- **Concurrency**:
  - `look` runs in `spawn_blocking` from the `watch` loop;
  - the gate reads `incident`, `disk_room_for` and `mac` from its own threads;
  - a `Mutex<Record>` protects the record, and each method takes it separately (no transaction across calls).
  - The loop wakes every 10 s (`TICK`), on `changed` (declarations, launches on volumes) and on **every domain
    revision** (`cards = domain.subscribe()`, `:1053`, `:1063`; `domain_runtime.rs:941-942`). The gauges are
    published in another loop every 10 s (`daemon-engine-machine/src/machine.rs:30-36`). A look may repeat the same
    `Machine`.

### M-ENGINE-12 · Waits declared for the machine (implicit, between the watch and the gate)
- **Kind**: implicit. **Defined in**: `Record.declared: BTreeMap<session, Declared{reason, resource, at, turn}>`
  (`machine_watch.rs:36-43`, `:61`); the entry comes in through `agent_waiting_machine`
  (`daemon-server/src/domain_bootstrap.rs:1262-1275`), which also calls `hold_back(.., Reason::Declared)`. The pause
  in the gate is part of M-ENGINE-2.
- **Source of truth / copies**: `declared` goes in `machine-incident.json` and the pause in `admission.json` (two
  copies of the same wait).
- **States**: declared, or forgotten. It is forgotten in `waiters` (`:882-904`) when the node has a later turn or
  `is_closed`, and in `Action::Release` (`:940-944`).
- **Notes**:
  - a session the engine no longer knows (no `turn` and no `is_closed`, for example after a restart) stays in
    `declared` forever. Harmless: it only supplies the names for a card that no longer exists;
  - `release_declared` is only called with `fixed`, which is dead (see M-ENGINE-11). `Declared` pauses depend only
    on their grace (M-ENGINE-2).

### M-ENGINE-13 · Disk stretches per volume and estimates per project (implicit)
- **Kind**: implicit. **Defined in**: `Ledger{usable, stretches: TaskId → Stretch{project, used, peak, whole}}`
  (`disk_peaks.rs:111-126`), `Ledger::sample_excluding` (`:136-157`), `share` (`:161-181`), `reserved` (`:185-191`),
  `Estimates` (`:51-102`).
- **Source of truth**:
  - Mac disk: `Record.stretches` and `Record.peaks`, **saved** (`machine_watch.rs:72-80`);
  - build volumes: `Record.build_volumes` (estimates, **saved**, `:104-106`), `candidate_peaks` (in memory) and
    `volume_stretches` (**`#[serde(skip)]`**, `:110-111`).
- **States of a stretch** (per task and volume):

| State | Encoding |
|---|---|
| No stretch | the task is not in `stretches` |
| Whole | `whole = true` (started with a previous reading) |
| Start not seen | `whole = false` (first reading of the ledger, or first after a restart with no saved ledger) |

- **Transitions**:
  - No stretch --(sample, task in `working`)--> Whole if `usable.is_some()`, otherwise Start not seen
    (`:149-154`);
  - --(sample, task no longer in `working`)--> No stretch; the peak is saved in `Estimates` only if it was whole
    (`:137-143`);
  - the split of what was lost or gained is in `share`;
  - `measure_volumes` creates the volume ledgers (`machine_watch.rs:614`) and discards them when the volume leaves
    the mount table (`:596`).
- **Invariants**:
  - "what they reserve covers what they can still take" (`disk_peaks.rs:7-9`);
  - "a working node carries on after a restart from what it took" (`:11-12`): only holds on the Mac disk. See
    F-ENGINE-7.
- **Concurrency**: under the `Mutex<Record>`. Only `pace` samples (the `look` thread).

### M-ENGINE-14 · Disk budget and reservations of the gate (implicit, in memory)
- **Kind**: implicit. **Defined in**:
  - `disk_working` and `disk_grants` (Mac disk), `volume_working` and `volume_grants` (per volume)
    (`machine_watch.rs:81-84`, `:118-121`);
  - hysteresis flags `short`, `boot_short`, `swap_short` and `volumes_short` (`:85-87`, `:89`, `:99`, `:124`);
  - `reserve_admission` (`:661-676`), `disk_reserved_for` (`:682-699`), `disk_room_for` (`:710-749`),
    `Record::volume_short` (`:152-208`).
- **Source of truth**: memory only. Everything is lost on restart, like `admitted` in the gate (M-ENGINE-1).
- **Events**:

| Event | What it does |
|---|---|
| `admit` → `reserve_admission` | adds a grant per key (`admission.rs:842-846`) |
| `disk_room_for` | puts the snapshot's `working` (which include the `in_flight`) into `disk_working`/`volume_working` |
| valid Mac disk reading (`measure_peaks`, `:636-649`) | clears `disk_working` and `disk_grants` |
| valid reading of a volume (`measure_volumes`, `:606-608`) | clears those of that volume |

- **Guards**: needed = reserved + estimate of the next one (0 if it goes alone, `alone`) + low line (5 GB). With 2 GB
  of hysteresis once short (`engine-ports/src/disk.rs:62-68`).
- **Concurrency**: `disk_room_for` takes the lock three times (`:712-718`), and `pace` can empty `disk_working`
  between the first and the third. See F-ENGINE-10.

### M-ENGINE-15 · Lifecycle of an observed volume (implicit, composite)
- **Kind**: implicit, composite. **Defined in**:
  - `volumes` (mounts where a node was launched, **saved**, `:62-64`);
  - `build_volumes` (learned, **saved**), `candidate_peaks`, `volume_spaces`, `volume_mounts` and
    `volume_readings_fresh` (memory);
  - `candidate_spaces` (`:551-582`), `measure_volumes` (`:584-631`), `watch_mount` (`:828-841`),
    `refuse_launch_on` (`:800-826`).
- **States of a volume**:

| State | Encoding |
|---|---|
| Unknown | in no set |
| Candidate | in `candidate_peaks` (memory only; measured while there are working nodes) |
| Learned | in `build_volumes` and in `volumes` |
| Watched by launch | in `volumes` without being in `build_volumes` |

  Presence, for the watched and the learned:

| Presence | Encoding | Effect |
|---|---|---|
| Mounted with budget | in `volume_mounts`, with a valid `volume_spaces` | normal |
| Mounted without budget | no valid reading | `refuse_launch_on` refuses every launch (`:806-817`); `volume_short` holds with `free: 0` (`:192`) |
| Confirmed absent | complete table without it | its restriction and its incident are suspended |
| Presence unknown | `volume_mounts = None` or the table reading failed | treated as present |

- **Transitions**:
  - Candidate --(a stretch takes ≥ `LEARNED_BYTES` = 1 GB of the volume)--> Learned (`:616-626`);
  - **there is no exit transition from Learned or Watched**: `build_volumes` and `volumes` are never pruned (only
    `candidate_peaks.remove`). See F-ENGINE-9.
- **Persistence and restart**: membership survives a restart; presence and budget do not. After a restart, every
  learned volume is Mounted without budget until the first look (documented in `admission.md`).

### M-ENGINE-16 · Handover of a `claude --bg` node to the Agent SDK (explicit, legacy)
- **Kind**: explicit. **Defined in**:
  - `Step = Planned → BgStopped → SdkAdmitted → Done` (`engine-ports/src/journal/handover.rs:7-34`);
  - insert-only table `sdk_handover_steps`, with triggers that prevent changing, deleting or skipping steps
    (`store-sqlite/src/handover.rs:8-24`);
  - reader `handovers` (`:85-113`);
  - engine `bg_handover::advance` (`engine-sdk/src/bg_handover.rs:83-102`), serialized with `Handovers.moving`
    (`engine-kernel/src/bg_sessions.rs:120-128`).
- **Source of truth / copies**: SQLite (`domain.db`). Derivatives:
  - `handover_of` and `successor` (`bg_sessions.rs:150-163`);
  - `sdk_handover::tell`/`follow`, which wait for `Done` to forward messages
    (`daemon-engine/src/sdk_handover.rs:36-73`);
  - `holds_messages` (`domain_transport.rs:387`).
- **Transitions and effects**:

| From | Step | Effect |
|---|---|---|
| Planned | `stop` | `claude stop`; then `record_bg_stopped` |
| BgStopped | `admit` | `continue_session` → `start_bound`; records `SdkAdmitted` in the same transaction as `AdmitExecution` (`sdk_owner.rs:587`) |
| SdkAdmitted | `finish` | adopts or launches, delivers the message it carried, closes the old node and records `Done` |

- **Terminals**: `Done` (the triggers make it irreversible).
- **Production**: no handover is planned any more. `plan_handover` is only called by a test
  (`bg_handover/tests.rs:204`), so only those left by an earlier version advance.
- **Missing guards**: `tick` (`bg_handover.rs:235-243`) retries every non-`Done` handover without checking whether
  its task has ended. See H-ENGINE-4.

### M-ENGINE-17 · Worktree ownership at launch (implicit, small)
- **Kind**: implicit. **Defined in**: `launches::own_worktree` (`engine-kernel/src/launches.rs:31-53`).
- **Source of truth, in two places**:
  - a `task_worktree` row in the store (direct write);
  - `Task.worktrees` in the domain (`RecordTaskWorktree`), both in `DomainRuntime::set_task_worktree`
    (`domain_runtime.rs:806-810`).
- **States**:

| State | Encoding |
|---|---|
| No worktree | neither copy |
| Registered | store and domain |
| Only in the store | the direct write went in and the command was rejected |
| Task ended while starting | the worktree is deleted, except unsaved work (the intent) |

  See F-ENGINE-8.
- The rest of `launches.rs` (validations, prompts, canonical context) is pure stateless functions.

Second-pass coverage B (M-ENGINE-18..27): `crates/engine-live/src/{node_reports,sdk_live,codex_live,contexts,node_work}.rs`
(without the `mod tests`), `crates/engine-kernel/src/domain_runtime.rs` (production code, lines 1-1238) and
`domain_runtime/{seals,problems,capabilities,batch_operations,rejections,inbox,observers}.rs`,
`crates/daemon-engine/src/domain_transport.rs` (production code, lines 1-1018). To follow each guard, these were also
read, only in the cited span: `daemon-engine/src/{domain_holds,domain_messages,idle_compaction,gate,wiring}.rs`,
`engine-sdk/src/sdk_owner.rs` (capability revocation and seal consumption), `store/src/lib.rs` (permit issue on
commit), `cli/src/args.rs` (`hook`), `plugin/hooks/{hooks.json,register.ts}`.

Context that changes the severity of everything in `domain_transport.rs`: **no node runs on `claude --bg` any more**
(`engine-sdk/src/bg_handover.rs:1-3`: "No node runs on `claude --bg` any more, so no new move is planned"), and the
hooks of an SDK node do not reach the daemon: `alondra hook` with `ALONDRA_SDK_HOST` only applies
`forbidden_for_nodes` locally and exits (`cli/src/args.rs:617-641`); the mod does not open a connection or a
`node hold` under the SDK either (`plugin/hooks/register.ts:288-293`, `:173`). What stays alive of that transport is:
sessions the user drives with the plugin, `--bg` sessions of an earlier release, and the sandbox (`register_probe`).
That is why the findings about that file carry low probability.

Paths about to change: `DomainRuntime::of` (`domain_runtime.rs:122-124`, the Bazel layers work (phase 4d-runtime)) and
`domain_transport.rs` moves to the GitHub port (the Bazel layers work (phase 5-adapters-out), point 3). No finding below
depends on those lines. Already covered elsewhere and not repeated here: M-STORE-1 (outbox), M-STORE-3 (read index
publication), M-STORE-4 (dispatches/certainty), H-ENGINE-1 (concurrent takes of the same host; confirmed as
F-ENGINE-12).

### M-ENGINE-18 · Serialized write and domain publication (`DomainRuntime`, composite)
- **Kind**: composite. **Defined in**: `crates/engine-kernel/src/domain_runtime.rs:13-73` (fields), `:896-957`
  (`commit`), `:989-1055` (`execute_bearer_once`), `:371-397` (`own_projected`), `:417-438` (`publish`), `:439-454`
  (`publish_batch_report`); `domain_runtime/observers.rs:11-32` (`CommitObserver`).
- **Source of truth**: SQLite (`store: RuntimeMutex<Box<dyn Journal>>`). **Copies and derivatives** in memory:
  `snapshot: RwLock<Arc<ReadSnapshot>>` (state + index of the same revision), `changed`/`schedule_changed`/
  `snapshot_changed` (watch), `seals` (M-ENGINE-19). Reads without a writer go through `reads` (a read-only WAL
  connection).
- **Write paths** (all under `store`):

| Path | Who | Publishes | `changed` | `observers.changed` | `observers.committed` |
|---|---|---|---|---|---|
| `commit` (`execute*`, `execute_as_system`) | daemon, clients | yes (`:937`) | if it changed and is not a replay (`:941-946`) | yes (`:949-951`) | yes (`:952-954`) |
| `execute_bearer_once` | nodes and orchestrator (MCP) | yes (`:1038`) | same (`:1042-1047`) | yes (`:1049-1053`) | **no** (documented in `observers.rs:29-31`) |
| `own_projected` | SDK owners (observations, controls) | yes (`:386`) | if `domain_changed` (`:388-393`) | **no** | **no** |
| `publish_batch_report` | orchestrator batches | yes (`:449`) | **no** | no | no |
| `carries_out` of an observer | `PauseResume` (Resume of the gate's pause) | no (no commit) | no | no | no |

- **Order inside a commit**: (1) test-only `before_next_commit`; (2) `carries_out` without taking `store`;
  (3) `store` → approval lease (user or transport only) → `authorize` → `domain_content::prepare` (copies
  attachments) → SQLite commit; (4) `publish` (`published` observers under the lock, replacement of the `Arc`,
  `seals.wake`); (5) `seals.put`; (6) `changed.send_replace`; (7) release `store`; (8) `changed`/`committed` of the
  observers **outside** the lock.
- **States per command**: not seen → committed (with receipt) | replayed (`replayed`, same `Arc`, `publish` does
  nothing because of `ptr_eq`, `:418-420`) | rejected (`StoreError::Domain`, attachments discarded `:933-935`) |
  storage failure (attachments **not** discarded, see notes).
- **Invariants enforced**: one publication per commit and in commit order (all under `store`);
  `OwnerCommit::acknowledge` requires what was published to be `ptr_eq` to the committed state
  (`engine-ports/src/journal/owner.rs:66-76`), so an owner's seals never come out before its state. Lock order:
  `authority` → `store` → lease (`bind_orchestrator :604-607`, `execute_bearer_once :996-1000`,
  `read_current_task_context :717-718`); no `store` → `authority` path was found.
- **Assumed**: that the `changed`/`committed` observers tolerate seeing a state older than the current one (they run
  outside the lock and two commits can be notified in reverse order). The three that exist (`Unattended`, `Retakes`,
  `ChosenAlways`) reread or are idempotent; no harm found.
- **Commands startup emits** (`open_observed :216-217`): `ReviewWaits` and `AcknowledgeRequestTasks`, with the
  observers already registered but the ports not yet wired (`EndedWaits` comes out because `domain.sdk()` is
  `None`). No harmful effect found.
- **Persistence and restart**: everything is rebuilt from SQLite (`load` + `ReadSnapshot::build`). Lost: `seals`,
  `rejections`, `held`, the orchestrator's counters.
- **Rejected**: typed (`StoreError::Domain`), except poisoned-lock panics, which shut the daemon down on purpose
  (`lock_or_exit`).

### M-ENGINE-19 · Seal queue for SDK owners (`Seals`, implicit, memory only)
- **Kind**: implicit. **Defined in**: `domain_runtime/seals.rs:9-127`.
- **Source of truth**: `clock::Monitor<Vec<Committed>>`. Each `Committed` carries dispatch and control permits,
  attempts, owner grants, recorded commands and observations (`seals.rs:96-105`).
- **States of a seal**: queued (per task) → taken by the owner of its task (`take`, `wait_for`) | forgotten
  (`forget_observations`, only observations of a host that ended, `:59-65`) | discarded (a `take` whose result is
  ignored: `engine-sdk/src/sdk_owner.rs:804`, `:900`).
- **Events**: `put` (only after publishing, `:21-24`), `take_of` (part per task, `:108-127`), `wake`.
- **Persistence and restart**: everything is lost. The store reissues dispatch and control permits on **every**
  commit that touches the task (`store/src/lib.rs:1539-1566`: `control_permits` of all `prepared`, and the permit of
  the next offer), and `reissue_permit` on adoption (`sdk_owner.rs:785`, `:880`). That is why a lost or discarded
  seal is recovered on the next commit that touches the task (checked; see the refuted list below).
- **Concurrency**: one monitor; owners wait with `wait_for` (condvar). Nothing prunes the seals of a task without an
  owner: they stay until an owner takes them (see H-ENGINE-9).

### M-ENGINE-20 · Producer capability registry (`Authority` + `domain-capabilities.json`, explicit)
- **Kind**: explicit. **Defined in**: `domain_runtime.rs:497-680`; `domain_runtime/capabilities.rs:33-130`.
- **Source of truth**: memory (`authority: RuntimeMutex<Authority>`) + file `domain-capabilities.json` (SHA-256
  digests only, `0600`, no links, atomic write with `fsync` of file and directory, `:98-130`).
- **States of a capability**: issued (memory and disk) → revoked (by digest, by agent, or end of the orchestrator's
  execution) | withdrawn on open (agent with no possible host, `live_agents :33-44`; earlier orchestrator executions,
  `:80-91`) | different epoch (the whole file is ignored, `:69-71`).
- **Guards**: `bind_agent` requires the agent to exist (`:524-526`) and **revokes the previous one of the same
  agent** before issuing (`:527`); with the registry full it withdraws those of agents without a host (`:528-533`).
  `persist_issued` forgets in memory what it could not write (`:592-594`); a revocation that is not written stays
  only in memory (documented decision `:589-591`).
- **Capacity card**: `raise_capacity_card` / `settle_capacity_card` (`:549-587`), check and raise outside the
  `authority` lock.
- **Persistence and restart**: the pruning in `load` is written before serving. An unwritten revocation (full disk)
  comes back to life on restart if the agent is still "alive" for `live_agents`.
- **Concurrency**: `authority` is held during a whole bearer commit (`:995-1000`), so a concurrent revocation does not
  overtake an already authorized write.
- **Revocation by agent vs. by digest**: `EndedCapability` revokes by digest (`sdk_owner.rs:644-652`), but
  `host_ended` (`:656-660`, from `lost`, `:1208-1211`) and `Unadmitted::drop` (`:670-677`) revoke **by agent**, that
  is, whatever capability is current, whoever holds it (H-ENGINE-8).

### M-ENGINE-21 · Streak of rejected cards (`Rejections`, implicit, memory)
- **Kind**: implicit. **Defined in**: `domain_runtime/rejections.rs:7-102`.
- **States**: `(owner, interaction_id) → n`. A domain rejection (except `no_effect`) adds; a success deletes the
  entry; on reaching exactly 3 it warns the orchestrator once with id `refused-{owner}-{card}` (`tell_orchestrator`,
  idempotent by id). An accepted question or delivery withdraws those warnings.
- **Persistence and restart**: lost (the streak starts from zero). No invalid states found; entries of a card that
  never succeeds are never deleted (small growth).

### M-ENGINE-22 · Alondra's own cards (`problems.rs`, `inbox.rs`)
- **Kind**: implicit. **Defined in**: `domain_runtime/problems.rs:8-199`, `inbox.rs:6-39`.
- **Idempotency**: the card id is the command id (`raise-{id}`, `tell-{id}`, `ask-{id}`, `notice-{id}`), so two
  concurrent raises of the same id resolve as a replay or an idempotency conflict in the store. `withdraw_problem`
  uses `withdraw-{id}-{revision}`.
- The prior `contains_key` check (`problems.rs:20-23` and equivalents) is only a shortcut: the store decides.

### M-ENGINE-23 · What is known live about a session (`Reported` in `Reports`, implicit, memory)
- **Kind**: implicit, composite. **Defined in**: `crates/engine-live/src/node_reports.rs:113-155` (state),
  `:256-335` (`record`), `:374-477` (`apply_on`, `apply_sdk`, `apply_codex`, `sdk_ended`, `connect`, `disconnect`).
- **Source of truth**: memory only (`Mutex<HashMap<String, Reported>>`); the domain never records it (`:1-7`). Three
  producers: the `claude --bg` mod (`node_event`, `node_connect`), the SDK host (`sdk_live::feed`, from
  `daemon-engine/src/wiring.rs:340`) and Codex's (`codex_live::feed`, `:341`).
- **Composite states** (bg): `known` (`started_at`) × `connection` (`connection: Option<u64>`) × `ended` × `in turn`
  (`working_since`/`tool`/`job`) × `background work` (`background`, `background_known`).
  - `connection_lost` = known ∧ ¬ended ∧ no connection (`:251-253`).
  - `halt` = in turn ∧ (session process dead ∨ Bash without a shell in two looks) (`:211-220`).
- **Transitions** (bg): `connect` → connected with a new number, `ended=false`, `working_since` per the hello
  (`:441-461`); `disconnect(n)` only if `n` is the current one (`:465-477`); `SessionEnd` → ended, no connection, all
  background `Stopped` (`:259-267`); `TurnStart`/`TurnComplete`; `ToolStart` replaces the single `tool` slot;
  `ToolEnd` empties it only if the id matches (`:329-333`).
- **SDK/Codex**: `sdk_live::observe` and `codex_live::observe` translate messages into the same `Reported`; `ended`
  clears tool, background and readings (`sdk_live.rs:238-249`, `codex_live.rs:136-141`).
- **What it decides**: only for bg nodes (`stopped_without_delivering`, `driven_by_user`, `node_revival` by `halt`).
  For SDK nodes it is display text: the state and label come from the domain
  (`daemon-server/src/views/live.rs:709-717`); `Reported` supplies tool, summary and background counts.
- **Persistence and restart**: lost. A bg node becomes a "node" again when its mod reconnects (retry with doubling
  wait up to 30 s, `register.ts:4-5`, `:213-219`). An adopted SDK node does not know the tool or background work it
  had in progress until the next message that names them.
- **Concurrency**: `node_event`/`node_connect` go through `hook_work`, which has **a single thread**
  (`domain_runtime.rs:189`), so bg reports are applied in order; `disconnect` and `set_peer` go outside it, under the
  same mutex.

### M-ENGINE-24 · Work of a node (`NodeWork`: subagents, workflows, finished tasks, steps; implicit)
- **Kind**: implicit. **Defined in**: `engine-live/src/node_work.rs:91-271`.
- **States per subagent/workflow**: running (`end: None`) → finished (`Finished|Failed|Stopped`); never reopened
  (`agent_started` on a finished one only updates the description, `:112-129`). Limits: 5 finished, 20 running
  (`keep_recent`, the oldest is discarded).
- **Steps**: `step_created`/`step_updated`/`steps_listed`; `Deleted` deletes; the whole list replaces.
- Display only. No findings.

### M-ENGINE-25 · Context measure and idle compaction (`Contexts` + `idle_compaction`, explicit)
- **Kind**: explicit. **Defined in**: `engine-live/src/contexts.rs:57-247`; `daemon-engine/src/idle_compaction.rs:46-120`.
- **Source of truth**: memory (`measured`) + `context-readings.json` (`last`: tokens, request time, cache,
  `compactable`). Saved whole and atomically; entries older than 7 days relative to the newest request are pruned.
- **States of a task's last request**: unknown → measured (`compactable=true`, after `Answered` with delivery time
  and cache) → spent (`spend`, compaction requested) | forgotten (`forget`: host ended, `compactable=false`) |
  deleted (`Compacted`, or `launched` with `StartKind` ≠ `Resumed`).
- **Transition that acts**: `compact_due` requests `CompactConversation` when the task rests (lifecycle `Resting`,
  resource available, no pause), has ≥ 200 000 tokens and ≤ 2 min remain before the cache expires. `spend` is
  recorded **before** the command runs (`idle_compaction.rs:82-91`): a refusal or a failure loses that compaction
  (documented decision: "at most once per rest").
- **Persistence and restart**: `last` survives; `measured` does not.

### M-ENGINE-26 · Hook transport and connection of the `claude --bg` mod (`domain_transport.rs`, composite, inherited)
- **Kind**: composite. **Defined in**: `daemon-engine/src/domain_transport.rs:42-532`.
- **Sub-machines**:
  - **Node connection** (`node_connection :278-348`): hello → `connect_node` (number) → writes the pending outbox
    lines once per connection (`pending_lines :382-401`, per-connection `sent` set) → mod ack → `hand_over` claims
    and ends as `Handed` (`:424-436`, under `store`). A newer connection replaces the previous one (`:301-303`,
    `:321-323`).
  - **Sustained card** (`node_hold :221-272`, `answer_after_recording :165-186`): published → waiting for answer |
    withdrawn if the hook goes away (per `withdraw_when_gone`) → answered.
  - **Held end of turn** (`hold_back :642-661`): counter `stop_blocks(task) = (n, marker)` in SQLite; reset when the
    marker changes (order of the node's last card); at `n ≥ 3` it lets it stop.
  - **Task start by first activity** (`start_assigned :541-557`): `StartTask` with the read's `expected_revision`; a
    conflict is only logged and the next hook retries it.
  - **Messages held for the next Stop** (`DomainRuntime.held`, `domain_messages::take_held`): no writer in
    production (F-ENGINE-11).
- **Source of truth**: outbox in SQLite (M-STORE-1), `stop_blocks` in SQLite, `Reports` in memory, `held` in memory.
- **Persistence and restart**: `stop_blocks` survives; connections and `Reports` are rebuilt when the mod
  reconnects; `release_orphans` releases at startup the sustained cards left without a hook (`:438-441`).
- **Concurrency**: everything that touches the domain goes through `hook_work` (one thread) or `guard_work` (two
  threads, read only for `PreToolUse`).

### M-ENGINE-27 · Wait report of an SDK node (`wait.json`, implicit)
- **Kind**: implicit. **Defined in**: `engine-live/src/sdk_live.rs:22-78`, `:755-761` (`EndedWaits`).
- **States**: absent → written by the node (`alondra report-wait`) → deleted on change of turn or execution
  (`clear_ended_waits`, in `published`, under the store lock) | ignored on read if its `pid` no longer exists.
- Display only (`read_wait_file` says so: "never an input to admission or lifecycle decisions").

### Findings

#### F-ENGINE-1 · The slot of an admitted start expires after 3 min even if the node does not exist yet: the gate admits above the limit
**Class**: CONFIRMED · **Severity**: S2 (work above the limit, recovers by itself) · **Probability**: low to medium.
It needs a launch that takes more than 3 min between admission and `AdmitExecution`. That span includes
`task_worktrees::ready`, `storage_for`, `isolate` (git worktree, maybe a fetch), `ensure_trusted` and
`require_initial_check`. It happens with large repos, slow disks or a slow network. · **Overlap**: the model
(`crates/machine-model`) reimplements the gate (`task-states.md:183-185`), so it does not see this. New evidence.
No related changes in the `git log` of `admission.rs`. No task in the related-coverage notes touches
`snapshot_locked` or `IN_FLIGHT`. Issues #374 (daemon response under load) and #334 (load test with a saturated
machine) create exactly the conditions in which a launch exceeds 3 min, but do not cover the slot. New evidence for
both.
- **Invalid state/transition and broken invariant**:
  - "A reservation has no timer: it holds while its node waits and its host lives"
    (`docs/architecture/admission.md:107-108`);
  - "at most `max_working_nodes` active nodes".
- **Evidence**:
  1. `admit` pushes `(key, now)` to `admitted` (`admission.rs:840-852`).
  2. `snapshot_locked` counts an admitted `Key::Task` only if `now - at < IN_FLIGHT` and
     `task.agent_id.is_none()` (`:1139-1151`).
  3. `working_tasks` only counts tasks with an agent (`slot_tasks` over `index.slots()`, `:1017-1022`). Between
     admission and `RegisterAgent`/`AdmitExecution` (`sdk_owner.rs:524-581`) the task has no agent.
  4. During the launch, `pass_at` is blocked waiting for `launch` (`:1395`). The other admitting callers stay
     active: `ask` from `auto_launch.rs:152`, `domain_schedule_runner.rs:412`, `provider_transition.rs:371` and
     `domain_orchestrator_tools.rs:639`, and `may_take_turn`, which calls `grant_turns` on its own
     (`admission.rs:462`).
- **Minimal sequence** (limit 1):
  1. A is in `launch_queue`. `pass_at` admits it and `launch` starts (`Launching::begin`, so `is_launching` is
     true and it does not count as `waiting_again`).
  2. A's `isolate` takes more than 3 min.
  3. At t = 3 min + epsilon, `auto_launch` calls `ask` for B: `working` = 0 (A is no longer in `in_flight` and has
     no agent) and B passes.
  4. A registers its agent: there are 2 active nodes with limit 1. The same happens with a turn granted by
     `may_take_turn`.
- **Variant from the other side** (phantom slot, same root): see F-ENGINE-2.
- **Impact**: work above the limit, which recovers by itself.
- **Deterministic test and ablation**: a test in `engine-admission`/`engine-sdk` with a real `Admission`,
  `Tuning { limit: Fixed(1), spacing: 0 }` and a virtual clock. Without launching anything: `ask(Key::Task(A), t0)`
  returns Ok and A still has no `agent_id`; then `ask(Key::Task(B), t0 + IN_FLIGHT)` must return
  `Err(Hold::Slots)`. Today it returns `Ok`. For the ablation, remove the `now - at < IN_FLIGHT` filter at `:1140`
  for `Key::Task` while the task is still in `asking`/`is_launching`.
- **Expected fix and files**:
  - the reservation of a `Key::Task` lives until its task has a node, or until an explicit `not_started` or
    `forget`, never by clock;
  - `IN_FLIGHT` stays only for the **memory** reservation (`observed_in_flight`), which is a time-based estimate;
  - files: `admission.rs` (`snapshot_locked`, `admit`, `carries_slot_over`) and `admission.md`.

#### F-ENGINE-2 · `auto_launch` admissions that never launch hold a slot for up to 3 min
**Class**: CONFIRMED · **Severity**: S3 · **Probability**: low. It depends on races with the claim of the row or on
a plan that disappears. · **Overlap**: issue #352 ("Close the create_task / auto-launch race for priority") touches
the same step of `auto_launch::pass`, but is about order by priority, not the slot that stays held. If #352 rewrites
that loop, the `Admitted` guard should go in there. New evidence. No task in the related-coverage notes covers it.
- **Invalid state/transition and broken invariant**: "A start the gate let through that did not start … holds no
  slot" (`admission.rs:536-539`, `not_started`).
- **Evidence**: in `auto_launch.rs:152-168`, after `gate().ask(...)` = Ok:
  - `claim_auto_launch` returns false and does `continue` (`:155-156`);
  - `begin_launch` returns `None` and does `continue` (`:158-161`);
  - `plan` is `None` and does `continue` (`:166-169`).
  None of them calls `not_started`. `settle` (`:177-208`) does not call it either for `usage_exhausted`,
  `plan_review_pending` or `conflict`. The `admitted` entry counts in `in_flight` while the task has no agent
  (`admission.rs:1147-1151`) until `IN_FLIGHT` expires it. Meanwhile, a repeated `ask` for the same key returns
  `Ok` without checking anything (`:526-528`).
- **Minimal sequence**:
  1. Limit 1. `auto_launch` calls `ask(A)` and gets Ok.
  2. `claim_auto_launch(A)` returns false because another pass or the user already claimed it.
  3. For 3 min, `ask(B)` gets `Hold::Slots { working: 1 }` with no node at all.
- **Deterministic test and ablation**: with the same gate double as F-ENGINE-1 and an `auto_launch` with a
  `test_support::Directory` store where `claim_auto_launch` returns false. After the pass, `snapshot.working` must
  be 0; today it is 1. For the ablation, add `not_started` in those three branches.
- **Expected fix and files**: every exit after an Ok `ask` that does not launch calls `not_started` (better, an RAII
  `Admitted` guard that does it in `Drop` unless confirmed). File: `auto_launch.rs`.

#### F-ENGINE-3 · The pause of a queued task lives in `admission.json`, apart from `launch_queue`: it can be lost, duplicated or stay forever
**Class**: CONFIRMED (the deterministic part); the part about a failure midway depends on a write failure ·
**Severity**: S2 (the launch is lost: the open task does not start by itself and does not show as paused) ·
**Probability**: "stays forever" is high every time a paused queued task is cancelled. "Lost or duplicated" is low:
the write of `admission.json` has to fail, for example with a full disk, a situation Alondra does live. ·
**Overlap**: none in the related-coverage notes or the open-issues list. New evidence. It is the same class of state
split between JSON and SQLite as F-STORE-4 (`closed-task-nodes.json`) and M-STORE-12.
- **Invalid state/transition and broken invariant**: a task is in the queue **or** paused, never in both and never
  in neither; nothing stays paused for a finished task.
- **Evidence**:
  - `hold_queued`: first `store.dequeue_launch(task)` in SQLite (`admission.rs:1230`) and then `gate.save(&inner)`
    in JSON (`:1252`). `save` only logs the failure (`:303-308`).
  - `release_queued`: first `enqueue_launch` (`:1270`) and then `held.remove` and `save` (`:1273-1274`).
  - Nobody else deletes `held` (search: only `release_queued`). `pass_at` does not filter `held` when launching
    rows of `launch_queue` (`queued_launches`, `:1071-1094`); only `ask` looks at it (`:522`).
  - `live()` publishes each `held` as `LivePaused { reason: User }` without checking whether the task is still open
    (`:626-635`). `start_failures::facts` marks `held: true` for the task (`start_failures.rs:17, 49`).
- **Minimal sequence**:
  1. Loss: the user pauses queued task T. `dequeue_launch` commits, `save` fails and the daemon restarts. T stays
     open, with no row and no `held`. No start takes it and it does not show as paused.
  2. Duplicate: the user resumes T. `enqueue_launch` commits, `save` fails and the daemon restarts. Now T has a row
     and `held`. `pass_at` launches it and it keeps showing as "paused by you" with the Resume button
     (`views/src/pacing.rs:104-106`) while it works.
  3. Forever: the user pauses T in the queue and then cancels it. `held[T]` stays in `admission.json` and in
     `LivePacing.paused` without limit.
- **Impact**: the launch is lost, or a task shows a wrong paused state, or a stale pause stays for a finished task.
- **Deterministic test and ablation**: a test in `engine-admission` with `test_support::Directory`.
  - For sequence 3: `hold_queued(T)`, cancel T in the domain, `pass_at`; `pauses().held` must not contain T. Today
    it does.
  - For sequences 1 and 2: make the gate's file not writable (a read-only directory inside the fixture) and check
    the invariant after `Admission::persisted_in`.
  - Ablation: remove the proposed cleanup.
- **Expected fix and files**: store the queue pause in SQLite, in the same transaction as the row (a `held_at`
  column in `launch_queue`) or as a domain fact. Meanwhile, `pass_at`/`lock_with` clean `held` of finished tasks and
  `queued_launches` excludes those in `held`.

#### F-ENGINE-4 · A turn with a granted slot loses its place in the queue after a restart
**Class**: CONFIRMED · **Severity**: S3 · **Probability**: medium on every restart with turns granted and not yet
taken, for example while their host is being retaken. · **Overlap**: none in the related-coverage notes or the
open-issues list.
- **Invalid state/transition and broken invariant**: "The age lives in `admission.json`" (`admission.md:99-100`);
  order by priority and age.
- **Evidence**:
  - `grant_turns` deletes `turn_waits[task]` and puts the task in `turns`, which is only in memory
    (`admission.rs:891-894`). It is saved without the age (`save`/`save_with`, `:463`, `:1352`).
  - After the restart, `turns` is empty and `settle_turns` does `turn_waits.entry(task).or_insert(now)`
    (`:925-926`).
- **Minimal sequence**: T waits for a turn since t0. The gate grants it the slot at t1. The daemon restarts before
  the owner takes it. On return, T waits with `since = now` and ends up behind the tasks of its priority that had
  been waiting since t0 < t < now.
- **Impact**: wrong order in the queue after a restart.
- **Deterministic test and ablation**: with `Admission::persisted_in` and a fixture folder, grant a turn, create
  another `Admission` in the same folder and check that T's `since` in the snapshot is t0. Today it holds the new
  clock.
- **Expected fix and files**: `grant_turns` keeps the age in `Saved` (for example
  `granted: BTreeMap<TaskId, Millis>`) until `settle_turns` discards it.

#### F-ENGINE-5 · `task-states.md` describes a timer that no longer exists and omits the ones that do
**Class**: CONFIRMED · **Severity**: S3, documentation, but it confuses the audit of deadlines · **Probability**: not
applicable · **Overlap**: `opening_deadline` falls under the stalled-node recovery work (silence recovery). For that task
it is new that the deadline is not in the policy. the stalled-node recovery work (phase 3) removes `UNOWNED_TAKE_EVERY` and
`take_working_unowned`, and phase-4 removes the `ANSWER` deadline: the table in M-ENGINE timers changes with them,
and the "no timers" rule of those phases is the criterion to decide the rest. Issue #327 (Mermaid diagrams in the
architecture docs) touches `task-states.md`: fix the list of deadlines in the same change.
- **Evidence**:
  - `docs/architecture/task-states.md:179-182` still describes `STRANDED_AFTER` (5 min, in `admission`). It was
    removed in `9abc65444` (#179, "Replace the stranded-task clock with an event") and there is no match in
    `crates/`.
  - At the same time, `:66-67` claims `obligation_policy` is the only place with deadlines, when at least
    `IN_FLIGHT`, `opening_deadline`, `RELAUNCH_WAITS`/`SILENT_HOSTS` and `UNKNOWN_RESET_WAIT_MS` (timers table
    above) decide transitions.
- **Expected fix and files**: update "Where the result differs from the plan" with the real list and decide, one by
  one, whether each deadline moves into `obligation_policy` or is documented as an exception.

#### F-ENGINE-12 · A retake in progress and the adoption of a model transition take the same host at once: a false "host lost" card, or two live `Owner`s for one agent (formerly H-ENGINE-1)
**Class**: CONFIRMED for the two branches described below (1 and 2). Branch 3 (the stale take ends the host that
the other one adopted) has one step not followed to the end, which is marked. · **Severity**: S2. A "host lost" card
appears while another owner works with the host, or there are two owners for the same agent. The store is not
corrupted: its generation fence rejects the stale owner's writes. · **Probability**: low. The user has to ask for a
model change while a retake of that task is already running: a user pause (`halt`), "Try again" on a host-lost card,
`wake_released`, or disk space coming back (`retake_waiting_for_space`). The window is the duration of `take_again`:
seconds. · **Overlap**: the Codex recovery work (phase 2) changes `take_host` in `sdk_owner.rs` (a continued host adopted with a
pending permit). It is the same function, but it does not deal with exclusion: the changes need coordination.
the stalled-node recovery work (phase 3) removes `take_working_unowned` and `UNOWNED_TAKE_EVERY`, which is another retake
path (this one already checks `unowned()`); it covers neither `take_again` nor `adopt_for_transition`.
the Bazel layers work (phase 5f) moves `sdk_owner/worker.rs`, not these functions. No open issue covers it. New evidence. The
`git log` of `sdk_owner.rs` and `provider_transition.rs` has no fixes for this.
- **Invalid state/transition and broken invariant**: "Another takeover of the task waits for this one to end"
  (`sdk_owner.rs:130`); at most one unfinished `Owner` per agent; the host-lost card only appears if no host carries
  the task.
- **What the store returns to the stale take** (what H-ENGINE-1 asked):
  - `owner.adopt` reads the current generation and does `reserve_owner(stamp, current)`
    (`store-sqlite/src/owner.rs:742-745`). It is a CAS: `UPDATE … WHERE owner_generation = expected`; if no row
    changes, `Error::StaleOwner` (`store/src/lib.rs:856-881`). Both reservations run in transactions serialized by
    the store mutex, so both succeed: the first yields g+1 and the second g+2.
  - Every later read or write with the old grant goes through `fence_owner`, which requires the current
    `owner_generation` to equal the grant's, or returns `StaleOwner` (`store/src/lib.rs:882-893`). `adapter_state`
    calls it first (`:1164-1166`).
  - A stale owner's observations and dispatches give `StaleOwner`/`StalePermit`. The owner treats them as "no longer
    the store's" and goes on silently (`stale()`, `sdk_owner.rs:2488-2491`, and the uses at `:1763`, `:1805`,
    `:1833`, `:1842`, `:1853`).
  - On the host, `attach` only accepts a generation greater than the stored one and closes the previous owner's
    socket (`agent-host/persistent-core.ts:156-168`). A lower or equal generation is rejected ("Owner proof
    rejected", `:162`).
- **Why nothing excludes the two takes**:
  - `take_again` takes `TakeoverTurn` (`sdk_owner.rs:1409`) and looks at `holds_execution` only when the retake is
    launched (`retake_with_policy`, `:1384-1385`; `halt`, `:1479-1480`). It does not look again inside the turn.
  - `advance` in `Phase::Draining` with `Kind::Model` does not look at `being_retaken`. It only does for
    `Move|Refresh` (`provider_transition.rs:438-440`). If `!owns_sdk_host(agent)`, it calls `adopt_for_transition`
    (`:445-458`), which goes to `take_host` without `TakeoverTurn` or `unowned()` (`sdk_owner.rs:763`).
  - `owns_sdk_host` only looks at `held` and `driven_by_hand` (`sdk_owner.rs:2886-2891`). It does not look at
    `in_hand` or `retakes`, so a half-done retake does not count as an owner.
  - `add_sdk_owner` only discards **finished** `Owner`s of the same agent and keeps the live ones
    (`sdk_owner.rs:2925-2934`).
- **Minimal sequence, branch 1: false host-lost card** (CONFIRMED):
  1. A retake A (`take_again` → `adopt` → `take` with `end_refused = true`, `:852-853`) reserves g+1 (`:876-885`).
  2. The user changes the model. `provider_transition::pass` reaches `Draining` and `adopt_for_transition` (B)
     reserves g+2.
  3. A reads `adapter_state` with g+1: `StaleOwner` (`:899`) → `Failed("reading the provider state")` →
     `not_taken` → `lost()` (`:1155-1157`).
  4. `lost()` sends `RecordHostLost` (`:1217-1224`), unless the task is paused (`:1212-1215`). The domain rule only
     checks the task and the reason (`domain-rules/src/rules.rs:5540-5545`): it does not know B carries the host.
     If B has not yet done `add_sdk_owner`, `lost()` also calls `host_ended(agent)` (`:1208-1211`), which revokes the
     agent's capability: this is the consequence described in H-ENGINE-8.
  5. B goes on: it adopts the host or relaunches it, and its `Owner` works with the host-lost card open.
  - If the two takes run the other way round (B reserves before A), A is the stale one and the user's transition
    ends in `abort()` (`provider_transition.rs:518-526`), with a retake that did succeed.
- **Minimal sequence, branch 2: two live `Owner`s** (CONFIRMED):
  1. A reserves g+1 and reads `adapter_state` (`:876-899`) before B reserves.
  2. B reserves g+2 and reads `adapter_state`: both reads pass the fence at their time.
  3. Both find `Found::Live` (`:915-918`). A calls `adopt_owner` with g+1 (`:939`) and the host accepts it. Then B
     calls `adopt_owner` with g+2: the host accepts it and closes A's socket (`persistent-core.ts:165-167`).
  4. With no dispatches or controls to reconcile (normal with the node idle), neither writes to the store before
     returning (`:966-991`: `receive_permits`, `sealed` and `unopened` only read or move permits in memory). Both
     return `Ok(Some(driver))`.
  5. `take_again` does `add_sdk_owner(A)` (`:1465-1466`) and the transition does `add_sdk_owner(B)`
     (`provider_transition.rs:450`, `:459`). Two unfinished `Owner`s for the same agent remain in `held`.
     `ask_to_release_host` and `owns_sdk_host` only see the first (`sdk_owner.rs:2936-2940`).
  - After that, A sees its socket closed. What its loop does then (replace the host with a new reservation, which in
    turn would close B's socket, or end in `lost()` after `SILENT_HOSTS`) was **not followed**.
- **Branch 3: the stale take ends the other's host** (step not checked):
  - Like branch 2, but B attaches before A. The host rejects A's g+1 (`persistent-core.ts:162`). If `adopt_owner`
    returns `Err` when the socket closes (not followed inside `provider-claude/src/adapter.rs:779`, `connect`), A has
    `end_refused = true` and does `end_holders` on the host's folder (`sdk_owner.rs:939-947`). That ends the host B
    has just adopted, and A retries with a g+3 reservation.
- **Variant inside the same race** (not followed): the grants come out of `domain.seals`, which is a per-task queue.
  A does `seals.take(&task)` after its commit (`:885-887`). If B's commit enters between the two, A can end up with
  B's grant and B with none ("the reservation issued no owner grant").
- **Impact**: a false host-lost card while a host works, an aborted user transition, or two owners for one agent.
- **Deterministic test and ablation**: an engine scenario with `engine-doubles::agent_host` and the existing barrier
  `after_owner_reserved` (`sdk_owner.rs:2954-2960`).
  - Branch 1: hold A at the barrier, advance the transition until B reserves, release A. Only one `Owner` must
    remain and no host-lost card. Today the card appears.
  - Branch 2: another barrier is needed, after `adapter_state`. With it, hold A, let B reserve and attach, release
    A. Only one live `Owner` must remain. Today there are two.
  - Ablation: remove the fix (taking `TakeoverTurn` in `adopt_for_transition`) and see both fail.
- **Expected fix and files**:
  - every take (`adopt_for_transition`, `account_move::finish`, `bg_handover::finish`) goes through
    `TakeoverTurn` and, inside it, checks `owned_now` and `holds_execution` again;
  - `advance` for `Kind::Model` respects `being_retaken`, like `Move|Refresh`;
  - `owns_sdk_host` (or a variant for the takes) counts `in_hand` and `retakes`;
  - `add_sdk_owner` rejects a second live `Owner` for the same agent;
  - files: `engine-sdk/src/sdk_owner.rs`, `engine-sdk/src/provider_transition.rs`.

#### F-ENGINE-6 · The watch advances the disk incident on every look, not on every sample: with watched volumes any domain change restarts the recovery (and without them one sample counts twice)
**Class**: CONFIRMED · **Severity**: S2. Tasks keep "waiting for disk" with the disk already recovered, and launches
are refused with "The disk is nearly full". It recovers by itself only when the domain stays quiet for about 10-20 s.
Without volumes: S3, because protection against an isolated spike is lost. · **Probability**: medium once there is a
disk incident (the tests cite real incidents of 7, 27 and 30 October and 9 November) and a watched or learned volume
(the tests' build cache volume). With working nodes the domain changes every few seconds; turns continue during the
incident because they only need a slot. · **Overlap**: none in the related-coverage notes or the open-issues list.
New evidence. No fixes in `git log`.
- **Invalid state/transition and broken invariant**:
  - "Measurements in a row past the recovery line that end an incident: one alone may be a blip"
    (`machine_watch.rs:31-32`);
  - the incident reflects the measured disk.
- **Evidence**:
  1. The `watch` loop looks again on every domain revision and on `changed`, not only by clock (`:1060-1064`).
     `sample()` returns the last published `Machine`, which only changes every 10 s
     (`daemon-engine-machine/src/machine.rs:30-36`).
  2. `look` sets `volume_readings_fresh = false` at the start (`:989`) and calls `pace` (`:990`).
  3. `pace` deduplicates the repeated sample and returns before `measure_volumes` (`:956`, with `sample()` at
     `:425-429`). `volume_readings_fresh` stays false.
  4. With non-empty `volumes` or `build_volumes`, `measured_volumes` is false and so is `measured_boot_disk`
     (`:992-996`).
  5. `step_with_disk_measurement` with a disk incident and `!measured_disk` sets `healthy = 0` (`:281-284`).
     `set_incident_on` saves it (`:1028`).
  6. Without volumes, `measured_boot_disk` is true, and the same healthy sample adds `healthy + 1` again (`:291`).
     At 2 it closes (`:293-299`).
  7. While it stays open:
     - `disk_room_for` returns `Some(Room)` because of `carded` (`:743-748`): the gate holds starts and returns;
     - `refuse_launch_on` refuses because of `disk_low()` (`:819-826`);
     - `domain_schedule_runner` pauses (`:398`, `:636`);
     - `retake_waiting_for_space` does not fire (`:1027`, `:1041-1044`).
- **Minimal sequence** (with a learned volume):
  1. Disk incident open. Sample S1 (`at = t`) arrives with the disk above the recovery line and the volume read:
     `healthy = 1`.
  2. A working node records a fact and the domain changes: a new look with S1, `healthy = 0`.
  3. Healthy sample S2: `healthy = 1`. Another domain change: `healthy = 0`. And so on while there is activity.
  - **Interleaving**: one `cards.changed()` between two gauge publications is enough. It also happens without
    activity, through the drift between two 10 s loops plus work: the watch looks twice between two samples when its
    work takes less than the probe. And the look itself causes revisions (`hold_for_disk` → `hold_back`).
  - **Variant without volumes**: the open incident receives healthy S1, with `healthy = 1`; a domain change repeats
    the look with S1 and `healthy = 2` closes the incident with a single measurement.
- **Impact**: tasks stay held on a recovered disk; or an incident closes on a single measurement.
- **Deterministic test and ablation**: a unit test in `engine-admission/src/machine_watch.rs` with `world()` and
  given volumes (`give_volumes`), no real clock.
  - Variant with a volume: open with `look(Machine{at: SAMPLE, ..machine(GB, 60)})`, learn the volume as in
    `learned_volume_cached_space_cannot_complete_incident_recovery` (`:1634-1662`) and give the volume a healthy
    reading.
    1. `look(at: 2*SAMPLE, healthy)` ⇒ `healthy == 1`.
    2. `look(at: 2*SAMPLE, healthy)` again: today `healthy == 0`; expected 1.
    3. `look(at: 3*SAMPLE, healthy)` ⇒ expected `incident().is_none()`; today it is still open.
  - Variant without volumes: `look(SAMPLE, low)`, `look(2S, healthy)`, `look(2S, healthy)` ⇒ expected to stay open
    with `healthy == 1`; today it closes.
  - Ablation: with the fix (do not advance the incident if `machine.at == previous sampled_at`), removing that guard
    makes both assertions fail.
- **Expected fix and files**:
  - `look` distinguishes "new sample" from "new look at the same sample": on the second it does not touch `healthy`
    or the freshness of the volumes, and it keeps processing `waiters`, suspension, `fixed` and `hold_for_disk`;
  - one option is for `pace` to return whether the sample was new and pass it to `step_with_disk_measurement`;
  - keep the `healthy` reset only for really failed readings, as the test at `:1648-1653` fixes;
  - file: `machine_watch.rs` (`look`, `pace`, `step_with_disk_measurement`).

#### F-ENGINE-7 · The stretches of build volumes do not survive a restart: working nodes then reserve at most 1 GB there
**Class**: CONFIRMED · **Severity**: S2: the gate admits too much on a build volume and builds fail for lack of
space. Recoverable. · **Probability**: medium to low. It needs a learned volume, several nodes mid-build and a daemon
restart (every published release restarts the daemon). · **Overlap**: none known. New evidence.
- **Invalid state/transition and broken invariant**:
  - "what they reserve covers what they may still take" (`disk_peaks.rs:7-9`);
  - "The stretches running and the latest reading are kept across a restart, so a node at work goes on from what it
    took" (`disk_peaks.rs:11-12`), with "Another volume builds write to is measured the same way" (`:17-19`).
- **Evidence**:
  1. `stretches` (Mac disk) is saved (`machine_watch.rs:77-80`); `volume_stretches` has `#[serde(skip)]`
     (`:110-111`). `build_volumes`, by contrast, is saved (`:104-106`).
  2. After a restart, the first `measure_volumes` creates an empty `Ledger` (`:614`). `sample` finds it without
     `usable` and puts every working task in with `whole = false` (`disk_peaks.rs:147-154`).
  3. `reserved` of a non-whole stretch gives `estimate.min(UNSEEN_START_BYTES) - used`, with `UNSEEN_START_BYTES` =
     1 GB (`disk_peaks.rs:185-191`, `:35`). `volume_short` uses that value for every working task
     (`machine_watch.rs:166-171`).
  4. The same happens when the volume briefly disappears from the mount table: `volume_stretches.retain` discards
     its ledger (`:596`), without saving the peaks of the stretches in progress.
- **Minimal sequence**:
  1. Learned volume with an estimate of 15 GB per node and 50 GB free. Three nodes have just started building
     (`used` ≈ 0): they reserve 45 GB and the gate does not admit a fourth.
  2. Daemon restart (self-update).
  3. First look: the three now reserve 1 GB each; needed is 3 + 15 + 5 = 23 GB < 50.
  4. The gate admits the fourth and the fifth; the builds fill the volume.
- **Impact**: over-admission on a build volume; builds fail without space.
- **Deterministic test and ablation**: unit test in `machine_watch.rs`.
  1. `MachineWatch::persisted_in(home, Disks double)`, seed `build_volumes[mount]` with `seed("app", &[15*GB; 3])`,
     `give_volumes([(mount, space(50*GB))])`.
  2. `measure_volumes(&working(t1,t2,t3), 0)` and again at 1 with the same reading.
  3. Check `disk_room(&working(..), Some("app"), 1).is_some()`.
  4. Reopen with `persisted_in`, give the same reading and `measure_volumes(&working(..), 2)`.
  5. Expected `disk_room(..).is_some()` (they still reserve 45 GB); today it is `None`.
  - Ablation: save the ledger (remove the `skip`) and revert it.
- **Expected fix and files**:
  - save `volume_stretches` with the volume's identity (for example `f_fsid` or the volume UUID, so as not to split
    the jump of another disk mounted at the same path) and discard it only if it changes;
  - when an unmount is confirmed, close the stretches with their peak instead of dropping them;
  - files: `machine_watch.rs` (`Record`, `measure_volumes`) and maybe the `Disk` port (volume identity).

#### F-ENGINE-8 · `own_worktree` does not delete the worktree of a task that ended while starting: the domain command rejects it before reaching that branch
**Class**: CONFIRMED. The deterministic part is `own_worktree` on an already finished task. The precondition is that
the task ends (for example the user cancels it) during `isolate` or `reopen`, which can take a while (git worktree,
fetch). · **Severity**: S3: an orphan worktree on disk until the next daemon start, and a misleading error
(`storage_unavailable`). · **Probability**: low. · **Overlap**: none. New evidence.
- **Invalid state/transition and broken invariant**: "A task that ended while the worktree was being made already had
  its close, so the worktree goes at once, unless it holds work not saved" (`launches.rs:33-34`).
- **Evidence**:
  1. `own_worktree` first calls `domain.set_task_worktree` (`launches.rs:41-42`).
  2. `set_task_worktree` writes the store row and then runs `Command::RecordTaskWorktree`
     (`domain_runtime.rs:806-810`).
  3. `RecordTaskWorktree` rejects a finished task with `task_not_active` (`domain-rules/src/rules.rs:5686-5688`).
  4. `own_worktree` turns that error into `storage_unavailable` and returns with `?` (`launches.rs:41-42`). It never
     reads the state nor reaches `release_recorded_worktree` (`:43-52`). That branch is only reached if the task ends
     in the instant between the command commit and `domain.state()`.
  5. The cancellation close (`release_cancelled`, `engine-worktrees/src/cancelled_worktrees.rs:66-71`) reads
     `task_worktree` on cancel. If it ran before the direct write, it did not see it. The startup sweep (`sweep`,
     `:143-170`) picks it up on the next start, and only for `Cancelled` tasks.
- **Minimal sequence**:
  1. T's launch starts and `isolate` begins.
  2. The user cancels T: its close finds no worktree.
  3. `isolate` ends and `own_worktree` fails with "could not record that T works in DIR: Task T has ended".
  4. The worktree stays on disk until the next start.
- **Impact**: an orphan worktree and a misleading error.
- **Deterministic test and ablation**: an engine test with the `Worktrees` double, whose `chosen` gives a directory
  with `.git` as a file. Create the task, cancel it and call `own_worktree(domain, task, Workplace::Made(dir))`.
  - Expected: `Err(task_not_active)` and a `release_recorded_worktree` call recorded in the double.
  - Today: `Err(storage_unavailable)` and no release.
  - Ablation: with the fix, put the original call back.
- **Expected fix and files**: read whether the task ended before recording, or treat `task_not_active` from
  `set_task_worktree` as "ended while starting" and release the worktree. File: `launches.rs`.

#### F-ENGINE-9 · A volume that is not a build volume can stay learned forever and hold every start and the turns
**Class**: CONFIRMED. The path is read in full; the trigger depends on how the user uses their disks. ·
**Severity**: S2: the gate waits for disk because of someone else's disk and there is no way out inside Alondra. It
is solved by unmounting or freeing that disk. · **Probability**: low. It takes copying to a local external disk, while
k nodes work, at least k GB (the split is equal), and that disk being nearly full. · **Overlap**: none. The test at
`:1668` documents only the case "filled without working nodes".
- **Invalid state/transition and broken invariant**: a volume no build writes to, in the Learned state of
  M-ENGINE-15, with no exit transition.
- **Evidence**:
  1. Candidates are the local volumes, writable and visible in Finder (`system/src/disk.rs:38-42`, `:130-139`).
  2. While there are working nodes, whatever the candidate's used space grows by is split among their stretches
     (`machine_watch.rs:614-615`; `disk_peaks.rs:166-174`).
  3. With a stretch ≥ 1 GB, the volume goes into `build_volumes` and `volumes` and is saved
     (`machine_watch.rs:616-629`). No path removes it afterwards.
  4. From then on:
     - `volume_short` demands on that volume the nodes' reservations + the estimate of the next one (≥ 1 GB) + 5 GB
       (`:152-208`);
     - `tightest_on` counts it for the disk incident (`volumes()`, `:843-853`; `look`, `:997`);
     - if it falls under the floor, `hold_for_disk` holds the next turn of every node but one (`:1000-1006`;
       `admission.rs:1049-1067`).
- **Minimal sequence**:
  1. With one node building, the user copies 2 GB of photos to an external disk with 6 GB free.
  2. The disk becomes learned.
  3. Every start needs ≥ 1 + 1 + 5 GB there: `Hold::Disk` while the disk stays mounted. And if it drops below 5 GB,
     an incident opens.
- **Impact**: starts and turns held for a disk that Alondra does not use.
- **Deterministic test and ablation**: a unit test with `give_volumes`.
  1. A candidate with a working stretch and 1 GB growth in used space: `build_volumes.contains_key(mount)`.
  2. With the disk at 6 GB free, `disk_room(..).is_some()` forever, also after many samples without nodes and after
     reopening.
  - It pins the current behaviour. The fix makes it expire.
- **Expected fix and files**: depends on a product decision. For example, a learned volume expires if it receives no
  writes in N stretches, or learning requires that the growth comes from the nodes' processes and not from the
  volume's total.

#### F-ENGINE-10 · `disk_room_for` decides "the next one goes alone" with a `disk_working` that `pace` can empty mid-call
**Class**: CONFIRMED, by interleaving between threads. There is no seam to reproduce it without a barrier. ·
**Severity**: S3. A start is admitted without counting its own estimate; the floor (`hold_for_disk`) remains as a
backstop. · **Probability**: very low. The window is microseconds, once per sample. · **Overlap**: none.
- **Invalid state/transition and broken invariant**: "The gate's head needs only a disk's line when nothing else
  reserves that disk" (`machine_watch.rs:708-709`).
- **Evidence**:
  1. `disk_room_for` takes the lock three times:
     - puts the `working` into `disk_working` (`:711-716`);
     - computes `reserved` in `disk_reserved_for`, which takes it again (`:717`, `:682-699`);
     - takes it a third time and computes
       `alone = head.is_some() && disk_grants.is_empty() && disk_working.keys().all(== head)` (`:718-719`).
  2. `measure_peaks`, in the `look` thread, empties `disk_working` and `disk_grants` (`:641-642`).
  3. The gate's snapshot calls `disk_room_for` from another thread (`admission.rs:1215`), with `reserving` without
     the head (`:1212`).
- **Minimal sequence (interleaving)**:
  1. The gate, in step 1, inserts t2's `working`.
  2. `look` → `measure_peaks` empties the map.
  3. The gate, in step 3, sees `disk_working` empty: `alone = true`, the estimate is 0 and what is needed stays at
     `reserved` + 5 GB. The correct value is `reserved` + estimate + 5 GB.
- **Impact**: a start is admitted without its own estimate.
- **Deterministic test and ablation**: refactor to compute everything under a single lock and derive `alone` from
  `working`. The unit test pins that `disk_room_for(&{t2}, p, Some(t1))` with an empty `disk_working` requires the
  estimate. Today it passes because of the reinsertion in step 1, so the ablation needs a test seam between steps 1
  and 3 (a `#[cfg(test)]` `Barrier`), or it is accepted as a structural fix.
- **Expected fix and files**: one critical section in `disk_room_for` (and `disk_detail_for`), and `alone` computed
  from `working` + grants. File: `machine_watch.rs`.

#### F-ENGINE-11 · Two states of the hook transport are unreachable and their documentation describes behaviour that does not exist
**Class**: CONFIRMED · **Severity**: S3 · **Probability**: always (it is dead code), cosmetic impact · **Overlap**: no
task or issue. `git log -S "problem-undelivered"` shows that id was born in `7c017f4f6` and `89c327ce0` (2026-10-09).
The withdrawal branch survived the change that stopped raising the card. New evidence.
- **Invalid state/transition and broken invariant**: the declared model ("user messages to a session in its terminal
  are delivered at its next Stop"; "on reaching the cap, the node's last words go in a card for the user") is
  implemented by nobody; the orchestrator reads a counter that can never be greater than 0.
- **Evidence**:
  1. `DomainRuntime.held` is documented as "User messages for sessions in the user's own terminal, handed at their
     next Stop hook" (`domain_runtime.rs:46-49`) and is created empty (`:202`).
  2. Its only production consumer is `take_held` (`domain_messages.rs:401-404`), called on Stop
     (`domain_transport.rs:518-521`), and the orchestrator's read `messages_held_for_turn_end`
     (`domain_orchestrator_reads.rs:237`).
  3. There is no writer outside tests: the only insertions are `domain_transport.rs:1503`, `:1509` and
     `domain_orchestrator_reads/tests.rs:116`. The Stop branch that returns `additionalContext` is unreachable.
  4. In `hold_back`, the `blocks > STOP_BLOCKS` branch that withdraws `problem-undelivered-{task}-{marker}`
     (`domain_transport.rs:648-652`) cannot happen: the counter's only writer is `set_stop_blocks(blocks + 1)`
     (`:659`), which only runs with `blocks < STOP_BLOCKS` (`:656`), so the stored value is at most 3. And nobody
     raises a card with that id: the only use of `undelivered_problem_id` is that withdrawal (`:650`, `:663-664`).
  5. The comment of `STOP_BLOCKS` says the node "may stop, and its last words reach the user" (`:559-570`). What
     really happens at the cap is that `start_failures.rs:42-43` reads `NodeFact::Stopped { sent_back: false }` and
     the case goes to the orchestrator (`Answer` obligation), not to a user card.
- **Minimal sequence**: not applicable (it is a state that cannot be reached). Any Stop of a bg node reaches
  `take_held`, which returns empty.
- **Impact**: none for the user today. It is a maintenance risk: a reader (or an auditing node) takes as real a
  message delivery that does not exist, and the orchestrator sees a field that reports nothing.
- **Deterministic test and ablation**: a `daemon-engine` test over `wiring::open` of a `test_support::Directory`.
  Run all the production paths that write messages for a session the user drives (`keep_for_task` with that session's
  task) and check that `domain.held` stays empty. Today it passes, and that is exactly what the finding shows. Better
  as an architecture test: delete `held` and everything compiles except the three cited tests. There is no ablation
  to do.
- **Expected fix and files**: remove `held`, `take_held`, the Stop branch, the field `messages_held_for_turn_end` and
  the `blocks > STOP_BLOCKS` branch with `undelivered_problem_id`; fix the comment of `STOP_BLOCKS` so it says where
  the case goes (orchestrator). Files: `engine-kernel/src/domain_runtime.rs`,
  `daemon-engine/src/{domain_messages,domain_transport,domain_orchestrator_reads}.rs`. The other option is to decide
  that the held delivery must exist and write it.

### Hypotheses

#### H-ENGINE-1 · Closed: confirmed as F-ENGINE-12
Two concurrent takes of the same host (a retake in progress and the adoption of a model transition). The question
"what does the store return to the stale take" and the sequences are answered in F-ENGINE-12.

#### H-ENGINE-2 · Double count of a slot: an admitted return plus a granted turn for the same task
**Severity if confirmed**: S3 (capacity is underused for a few seconds). **Overlap**: none in the related-coverage
notes or the open-issues list.
- **Path**:
  1. `pass_at` admits `Key::Resume(s)`: `resume_locked` → `hand_pauses` → `Resumed` in the domain.
  2. The domain change wakes another pass **before** the owner calls `may_take_turn`.
  3. `grant_turns` puts T in `turns` (`admission.rs:884-897`).
  4. `snapshot_locked` counts T in `in_flight` (Resume not `at_work` and not busy, `:1143-1146`) **and** in
     `turning` (`:1158-1159`).
  5. When the owner arrives, `take_granted` wins over `carries_slot_over` (`:443-466`), so the return never becomes
     `at_work` and the double count lasts until the turn starts.
- **Missing to confirm**: that after `Resumed` of an SDK node the lifecycle ends up with `input_starts_turn()`
  (`domain/src/lifecycle.rs:1443-1445`, which requires `resource == Available` and `InputPending`) in the case of a
  host replaced by `halt` or released.
- **Proposed test**: a test in `engine-admission` with a fixture domain. User pause, `resume_now`, `pass_at` twice
  and, before `may_take_turn`, check `snapshot.working`.

#### H-ENGINE-3 · The user's "Resume" is lost if the domain rejects `Resumed`
**Severity if confirmed**: S3 (the user can press again, because the card is still open). **Overlap**: none in the
related-coverage notes or the open-issues list.
- **Path**:
  1. `resume_locked` deletes `resumed[session]` and the internal pause (`admission.rs:801-809`).
  2. `admit` counts the return.
  3. `hand_pauses` sends `Resumed`. If the domain rejects it, it is recorded in `refused` (`:780-786`) and the pause
     stays in the domain.
  4. As it is `ByUser`, `returns()` gives false forever (`engine-ports/src/admission.rs:103`).
  5. `PauseResume` already answered "The paused node resumes now." (`daemon-engine/src/gate.rs:21-26`).
- **Missing to confirm**: a reachable rejection of `Resumed` on a user pause (`not_paused` does not apply because
  there is a pause; it would have to be seen in `end_of_pause`/`decide`, `node_machine.rs:524-534, 573-588`).

#### H-ENGINE-4 · A `--bg` → SDK handover whose task already ended is retried forever, and each attempt wakes the next
**Severity if confirmed**: S2 (CPU, growth of agent rows, held messages). Probability low. **Overlap**:
the tests-by-layer work (host model) (points 22 and 35) moves the `bg_handover` tests, but not this guard.
- **What is known**:
  - `tick` retries every non-`Done` handover without looking at its task (`bg_handover.rs:235-243`);
  - in `BgStopped`, `admit` → `start_bound` runs `RegisterAgent` as its own commit (`sdk_owner.rs:526-530`) before
    `AdmitExecution` fails with `active` for a finished task (`rules.rs:5433-5434`). `active_account_immutable` also
    fails if the task moved to another account or provider (`:5446-5447`);
  - the commit changes the revision (`domain_runtime.rs:941-942`), and `run` wakes with it (`bg_handover.rs:225-231`):
    a continuous loop of `RegisterAgent` + bind and revoke capability + configure the worker;
  - meanwhile, `sdk_handover::tell`/`follow` leave a tokio task waiting for `Done` per message
    (`sdk_handover.rs:50-73`) and `holds_messages` holds that node's inbox.
- **Missing to confirm**:
  1. That real databases still have non-`Done` handovers: query `sdk_handover_steps` for nodes without a `done` row.
     No new ones are planned any more.
  2. That `Worker::configured` does not launch a process per attempt.
  3. That `require_initial_check` does not cut earlier (in that case the retry is every 10 s, with no loop).
- **Proposed test**: an `engine-sdk` test with the host double and the `test_support::Directory` store. Insert
  `planned` + `bg_stopped` for a cancelled task, advance two `tick`s and check that the handover closes (or is marked
  without retry) and that no agent is registered.
- **Probable fix**: in `tick`/`advance`, a handover whose task ended is closed without admitting. That needs a new
  step, because the triggers demand order; for example, allow `done` without `sdk_admitted` with a reason. Treat
  `task_not_active` as the end of the handover.

#### H-ENGINE-5 · A watched volume that stays mounted but cannot be measured blocks launches and incident recovery forever
**Severity if confirmed**: not stated in the report. **Overlap**: none stated.
- **What is known**: if `space()` fails (`statvfs` with an I/O error on a failing disk) or the double check of
  `mount_point` does not match (`machine_watch.rs:565-570`):
  - the volume is missing from `spaces` and `volume_readings_fresh` is false on every look (`:603-604`);
  - `refuse_launch_on` refuses every launch (`:806-817`);
  - `volume_short` holds with `free: 0` (`:192`);
  - an open disk incident never recovers (`:281-284`).
  This agrees with `admission.md` ("waits for a valid reading"), but there is no card (incidents no longer open
  any) and no way out for the user except unmounting.
- **Missing to confirm**: under which real conditions `statvfs` or `getattrlist` fail on a present local mount. With
  a hung network mount, `statvfs` may **block** instead of failing, and would stop the whole `watch` loop, because
  `look` runs in `spawn_blocking` and is awaited (`:1057`).

#### H-ENGINE-6 · After a daemon restart, a `--bg` node that has not reconnected yet is not a "node", and its plan or question falls to the terminal
**Severity if confirmed**: S2 (a task stopped on a terminal question nobody reads, or a plan approved by another plan
hook on the machine). **Probability**: low: only `--bg` sessions of an earlier release remain. **Overlap**: none.
- **Path**:
  1. `Reports` lives only in memory (`node_reports.rs:349-361`). After the restart, `is_node(session)` is false until
     a `session.start` or a `node_connect` arrives (`:537-539`).
  2. The mod retries the connection with doubling wait up to 30 s, and only returns to 1 s on receiving `connected`
     (`register.ts:4-5`, `:207`, `:213-219`). The window lasts up to 30 s after the daemon is back.
  3. An `ExitPlanMode` or an `AskUserQuestion` in that window calls `node hold` → `post(…, is_node = false)` →
     `Posted::NotHeld` (`domain_holds.rs:110-112`) → `{"held": false}` (`domain_transport.rs:234`) → the mod goes on
     with Claude Code's normal flow (`register.ts:413`, `:420`). This contradicts the comment at
     `domain_holds.rs:90-94` ("a node's plan never falls through to the other plan hooks"). Also, in that window a
     permission of a cancelled task is not denied.
- **Missing to confirm**: what Claude Code `--bg` does with an `ExitPlanMode`/`AskUserQuestion` without an answer from
  the plugin (asks in the terminal or denies by itself), and whether `job.phase == Blocked`
  (`domain_transport.rs:609`) ends up sending the node back.
- **Proposed test**: a `daemon-engine` test with `fixtures::node_with_task`, without a prior `connect` (equivalent to
  a freshly opened daemon) and a `node_hold` of kind `plan`. Today it answers `held: false`; the right answer is
  `held` or `Denied`. Ablation: decide `background` by "the agent is the node of an open task", taken from the domain
  and not from `Reports`.
- **Probable fix**: in `node_hold`/`hold_elicitation`, deduce "is a node" from the domain (agent assigned to an open
  task) when `Reports` does not know yet. Or remove the whole bg path if it is confirmed that no `--bg` sessions
  remain.

#### H-ENGINE-7 · Codex leaves "in progress" calls of an already finished turn and shows them as the next turn's tool
**Severity if confirmed**: S3 (row text). **Overlap**: none stated.
- **Path**: `turn/completed` records the end of the turn and moves the open `exec_command`s to the background, but
  does **not** empty `readings.calls` (`codex_live.rs:115-124`). In the next turn, the first `item/started` sets
  `session.tool = readings.calls.values().next()` (`:93`), which is the lowest id of the `BTreeMap`: it can be an old
  call that never received its `item/completed`. With 64 such calls no new one fits (`:90`). Only the end of the host
  clears them (`:140`).
- **Missing to confirm**: that the Codex app-server can close a turn (especially `interrupted`) without an
  `item/completed` for each open `mcpToolCall`/`fileChange`/`commandExecution`.
- **Proposed test**: a pure `engine-live` test with `Reports::apply_codex`. Do `threadReady`, `turn/started` T1,
  `item/started` of an `mcpToolCall` with id `a`, `turn/completed` T1 `interrupted`, `turn/started` T2 and
  `item/started` of a `commandExecution` with id `b`. Today `session.tool` is `a`; it should be `b`. Ablation: empty
  `calls` on `turn/completed`, except the `exec_command`s that move to the background.
- **Probable fix**: on `turn/completed`, remove from `calls` everything that does not move to the background, and pick
  the displayed tool by most recent `since`, not by id.
- **Variant in Claude SDK** (same root, a single `tool` slot): with parallel calls, if the result of the last arrives
  before that of another, `tool` is left empty while the other goes on (`node_reports.rs:329-333`,
  `sdk_live.rs:384-396`). It is not known whether the SDK emits each `tool_result` in its own `user` message and in
  arrival order.

#### H-ENGINE-8 · Revoking "the agent's capability" instead of "the one this host had" can leave a host that has just started without a credential
**Severity if confirmed**: S2 (the new node gets `forbidden` on all its Alondra tools, so it can neither ask nor
deliver). **Overlap**: H-ENGINE-1 / F-ENGINE-12. What is here is a new consequence of that race.
- **Why it is possible**: the same `AgentId` (the session) is reused between a task's hosts, and `bind_agent` issues a
  new capability for that agent (`domain_runtime.rs:521-547`). `host_ended`, which calls `lost()` if
  `!owns_sdk_host(agent)` (`sdk_owner.rs:1208-1211`), and `Unadmitted::drop` (`:670-677`) revoke **by agent**
  (`revoke_agent`, `domain_runtime.rs:651-659`), not by digest like `EndedCapability` (`sdk_owner.rs:644-652`).
  - Between `bind_agent` and `add_sdk_owner` of a take, `owns_sdk_host` is false.
  - A `lost()` or an `Unadmitted` of another take of the same task that runs in that window revokes the capability
    the first has just passed to its host.
- **Missing to confirm**: it has the same requirement as H-ENGINE-1 (two concurrent takes of the same host). That
  race is now confirmed as F-ENGINE-12, whose branch 1 reaches `lost()` → `host_ended(agent)` while another take
  holds the host: the precondition of H-ENGINE-8 is reachable. What remains to check is the rest of H-ENGINE-8 (that
  the revocation leaves the new host without a credential).
- **Proposed test**: with `engine-doubles::agent_host`, a barrier at `after_owner_reserved` of take A, after its
  `bind_agent`; run `lost()` of the same task, release the barrier and check `resolve_bearer` with A's capability.
  Today it would give `forbidden`.
- **Probable fix**: `lost()` and `Unadmitted` revoke the digest they issued, like `EndedCapability`, never
  `revoke_agent`.

#### H-ENGINE-9 · The seals of a task without an owner pile up in memory and every owner walks them on waking
**Severity if confirmed**: S3 (memory and CPU). **Overlap**: none stated.
- **Path**: every commit that touches an SDK task with a pending offer or a prepared control puts a `Committed` in
  `Seals` (`store/src/lib.rs:1539-1566`, `domain_runtime.rs:938-940`). Only a `take_of` by an owner of that task takes
  it out (`seals.rs:108-127`). If the task has no owner (host released and paused, host-lost card open), its seals
  stay, and `wait_for` of **all** owners walks the whole queue on every turn of the loop (`:31-42`).
- **Missing to confirm**: measure whether on a normal day there are tasks often touched without an owner (for example
  with messages while paused).
- **Proposed test**: an `engine-kernel` test: `put` N `Committed` of a task `t-x` without an owner and check the
  queue size. The right result would be that it stays at the last permit of `t-x`, because the store says the last
  one replaces the earlier ones (`sdk_owner.rs:1737-1745`).
- **Probable fix**: in `put`, replace the earlier dispatch permit of the same task instead of accumulating it.

### Refuted or no problem (so they are not repeated)
- **SDK-node hooks that revoke their capability with `SessionEnd`**: does not happen. `alondra hook` with
  `ALONDRA_SDK_HOST` does not call the daemon (`cli/src/args.rs:634-641`).
- **Seals discarded by `domain.seals.take(&task)` without using the result** (`sdk_owner.rs:804`, `:900`): a
  `TellNode` or an interrupt committed in that window loses its permit, but the next commit that touches the task
  issues them again (`store/src/lib.rs:1539-1566`), and the freshly installed host confirms observations right away.
  It does not get stuck.
- **`compact_ended_tasks` in an infinite loop**: `compacted()` is idempotent (`domain-rules/src/compaction.rs:113-139`),
  so a second pass sees nothing to change.
- **`live_agents` withdraws the capability of a node that is still closing**: a task only becomes terminal after
  draining its execution (`Drained::Execution`, `domain-rules/src/lifecycle.rs:38-46`), so no live host needs the
  credential.
- **Retry of "Resume" with the answer lost (`PauseResume::carries_out` leaves no receipt)**: the server's action
  journal repeats the result by action id (M-STORE-7), so the app does not reach `carries_out` again.
- **`execute_bearer_once` without `observers.committed`**: it is documented, and the commands those observers react
  to (`TellNode`, `AnswerQuestion` with `RETAKE`/`ALWAYS`/`resume`, `SetModel`…) are only emitted by the daemon or
  the user.
- **Lock order**: no path takes `store` and then `authority`.

Minor notes without finding status:
- A storage failure (not a domain one) in `commit`/`execute_bearer_once` does not call
  `prepared.discard_uncommitted` (`domain_runtime.rs:933-936`, `:1030-1033`), so the copied attachments stay until
  file retention (#503).
- `raise_capacity_card` checks and creates outside the `authority` lock (`:558-575`), and its id carries
  `now_millis`. Two starts that fail for capacity at the same time can leave two cards (S3, very rare since #364).

## Persistence, journals, deferred effects and restarts (`STORE`)

Scope: `crates/store`, `crates/store-sqlite`, `crates/domain-store-schema`, the outbox and its consumers,
`delivery_attempts`, `operation_journal` / `action_results` / `service_receipts`, the provider observation journal,
read index publication, startup (replay, preflight, migrations), JSON state files in the home folder, and the relaxed
durability used by tests. Audited at commit `1567967b9`. The overlap of each finding was checked against the
related-coverage notes and the open-issues list in a second pass, which also confirmed F-STORE-4.

### M-STORE-1 · Domain message outbox (`domain_outbox` + `delivery_attempts`)
- **Kind**: explicit in SQL (state CHECKs); transitions are implicit in code (no transition trigger).
  **Defined in**: `crates/domain-store-schema/schema.sql:71-74`; written in the command commit
  `crates/store-sqlite/src/command.rs:386-409`; claim/finish/resolve in `crates/store-sqlite/src/lib.rs:953-990`
  (`claim_in`), `:1755-1782` (`finish_message`), `:1814-1855` (`resolve_uncertain_authenticated`), `:1876-1895`
  (`resolve_uncertain`), `:1625-1639` (expiry); recovery on open `:724-731`; orchestrator batches
  `crates/store-sqlite/src/batch_recovery.rs:57-104`.
- **Source of truth**: SQLite (`domain_outbox.state` = state of the last attempt; `delivery_attempts` = history per
  attempt with `version`). **Copies and derivatives**: `orchestrator.json.active_run.attempts` (attempt id + version,
  JSON), in-memory maps `retry_after`/`first_seen` of the worker (`crates/daemon-server/src/domain_delivery.rs:31-33`,
  lost on restart, harmless).
- **States**: outbox `pending | dispatching | uncertain | handed | retryable | discarded`; attempt
  `dispatching | uncertain | handed | retryable | discarded`.
- **Events, guards, effects / allowed transitions**:

| From | Event [guard] | To | Effect |
|---|---|---|---|
| — | command commit with a `DomainEffect` | pending | row in `domain_messages` (+ `domain_message_user_targets` if `NotifyUser`), same transaction |
| pending/retryable | `claim_in` [orchestrator message, not a user one, `at` < now−24 h] | discarded | none |
| pending/retryable | `claim_in` | dispatching | new attempt `dispatching` v1 |
| pending/retryable | `discard_expired_orchestrator_messages` [orchestrator, >24 h] | discarded | — |
| dispatching | `finish_message(id, v)` [attempt `dispatching` and same version] | uncertain/handed/retryable/discarded | version+1 |
| dispatching | `DomainStore::open` (restart recovery) | uncertain | attempt v+1, `daemon_restarted_after_claim` |
| uncertain | `resolve_uncertain[_authenticated]` [same version] | retryable / discarded | v+1, `explicit_user_reconciliation`; the authenticated variant stores `local_command_results` |
| dispatching/uncertain (batch) | `settle_orchestrator_batch` | handed/discarded/retryable | all attempts of the batch |

- **Rejected**: `finish_message` with a different version/state returns `Ok(false)`; the user worker turns that into
  an error (`domain_delivery.rs:100`, `ensure!`) that takes down the worker and the daemon
  (`crates/alondrad/src/lib.rs:238-243`). `resolve_uncertain_authenticated` returns a typed `delivery_uncertain`.
- **Terminals**: `handed`, `discarded`. `retryable` is not terminal (it goes back to `dispatching`). Nothing reopens
  `handed`.
- **Consumers**: users and SDK nodes → `domain_delivery::run` (`domain_delivery.rs:25-104`); `claude --bg` nodes →
  `domain_transport`; orchestrator → `engine-orchestrator/src/domain_orchestrator.rs` (batches); `--bg`→SDK handover →
  `handover.rs:58-67` (claim and finish in two transactions; a cut between them leaves `uncertain`, conservative).
- **Invariants**: enforced: states by CHECK; a single live attempt per message in practice (the claim requires
  `pending/retryable`); ids never reused (there is no `DELETE` of `domain_messages`/`delivery_attempts`).
  **Assumed, not enforced**: that only one process opens the database as writer (see F-STORE-1).
- **Persistence and restart**: `dispatching` → `uncertain` (never redelivered just because of a restart). Delivery to
  an SDK node is idempotent by `command_id = tell-outbox-{message.id}` while the receipt lives (one week). User push:
  `uncertain` counts as already notified (`user_push_handed`, `lib.rs:1584`).
- **Concurrency**: everything under the store `Mutex` (`DomainRuntime.store`) and `IMMEDIATE` transactions.

### M-STORE-2 · Command commit and idempotency (`command_audit`, `semantic_receipts`, `local_command_results`, `command_bodies`)
- **Kind**: implicit (existence of rows). **Defined in**: `crates/store-sqlite/src/command.rs:42-450`,
  `lib.rs:898-922`, retention `crates/store-sqlite/src/retention.rs`.
- **States per command**: not seen → committed (audit + body + receipt [+ local result]) → receipt expired (>7 days:
  no body or receipt, audit keeps the type). `local_command_results` never expires.
- **Guards**: same `(actor, command_id, epoch)` with a different digest → `idempotency_conflict`; same → returns the
  stored result (`replayed: true`) without effects. Rejected commands write no receipt.
- **Persistence and restart**: everything in the same SQLite transaction as the domain change and the outbox
  (atomic).
- Note: `local_command_results` has no retention policy (`retention.rs:31-40`): it grows without limit (S3, not an
  invalid machine).

### M-STORE-3 · Read index publication (`ReadSnapshot`)
- **Kind**: explicit (a state+index pair of one revision). **Defined in**:
  `crates/engine-kernel/src/domain_runtime.rs:417-437` (`publish`), `:880-942` (`commit`), `:368-389`
  (`own_projected`), `crates/domain-rules/src/read_index.rs:457` (`pair`, `stale_revision`).
- **Source of truth**: SQLite; in-memory derivative `snapshot: RwLock<Arc<ReadSnapshot>>`; store cache
  (`Cached{state,data_version,total_changes}`).
- **Transition**: SQLite commit → (under the store mutex) `publish` builds the index and replaces the `Arc`; readers
  get a coherent `Arc`. `live::Facts::observe` invalidates if the reader's `max(revision)` > the published revision
  (`crates/daemon-server/src/views/live.rs:180-187`).
- **Result**: no mix of revisions served and no lost revision found: all publications happen under the same mutex and
  in commit order; a repeated command returns the same `Arc` (`ptr_eq`).

### M-STORE-4 · Host dispatches (`lifecycle_dispatches`) and certainty (`provider_receipt_proofs`)
- **Kind**: explicit in SQL (trigger `lifecycle_dispatch_transition`, `crates/store/src/schema.sql:94-100`).
- **States**: `prepared → claimed → handed | rejected | uncertain`; `prepared → revoked`. View
  `lifecycle_dispatch_certainty` (`crates/store/src/receipt_proofs.rs:4-27`): an `uncertain` with agreeing proofs
  reads as `handed`/`rejected`.
- **Deferred effect**: the permit (`DispatchPermit`) is issued after the commit (`crates/store/src/lib.rs:1575-1590`);
  a cut before sending leaves `prepared`, which `recover_prepared` reissues. A `claimed` without a receipt is resent
  with the exact bytes on adoption (`unresolved_commands`, `lib.rs:1321-1343`); with no recorded command, no host
  received it (`unrecorded_commands`).
- **Terminals**: `handed/rejected/revoked`; `uncertain` is terminal in the table, certainty is complemented with
  proofs.
- **Invariants**: a `Completed/Cancelled` task cannot commit with open dispatches (`UnresolvedDispatch`,
  `lib.rs:1547-1552`).

### M-STORE-5 · Provider observation journal
- **Kind**: explicit (gapless cursor + monotonicity triggers). **Defined in**: tables `provider_observations`,
  `provider_observation_heads`, `provider_normalization_checkpoints` (`crates/store/src/schema.sql:166-214`),
  `provider_discarded_observations` (`crates/store-sqlite/src/discarded_observations.rs`).
- **Transitions**: `accept_observation` (`crates/store/src/lib.rs:1092-1133`): same cursor and digest → `Duplicate`;
  different digest → `ObservationConflict`; cursor ≠ head+1 → `ObservationGap`. Checkpoint (`lib.rs:1243-1297`) only
  advances, only to the head, with `expected_previous`. Retention frees bytes below the checkpoint (trigger
  `provider_observation_immutable`) and deletes those of non-current executions (`crates/store/src/retention.rs:33-50`).
- **Result**: cursors without jumps or undetected duplicates; after deleting rows of non-current executions,
  `accept_observation` already rejects with `StalePermit` before looking at the cursor.

### M-STORE-6 · Orchestrator batches in the store (`orchestrator_batch_runs/effects/reports/tool_operations/decisions`, `orchestrator_deliveries`)
- **Defined in**: `crates/store-sqlite/src/batch_effects.rs:6-12`, `batch_recovery.rs`.
- **States**: run `ended 0|1`; tool operation `started → succeeded | uncertain`; reports with a gapless cursor
  (`record_batch_report`, `batch_effects.rs:168-188`); effects pin their receipts (`pinned`) until `finish_batch`.
- The recovery logic (what the orchestrator does with these states on restart) belongs to the orchestrator area.

### M-STORE-7 · Local action journal (`action-results.sqlite`) and operation journal (`operation_journal`)
- **Defined in**: `crates/daemon-server/src/views/action_results.rs:46-51, 195-345`,
  `crates/daemon-server/src/operation_journal.rs:141-502`.
- **Kind**: explicit (`state` column) in a SQLite database **different** from `domain.db`.
- **States**: `executing → completed | interrupted (startup) | retired`. Operations: `pending_count`, ranges retired
  per sender and generation; a change of binding/epoch → new generation and deletion of the sender's rows.
- **Deliberately split state**: the effect lives in `domain.db` and the result in `action-results.sqlite`; a cut
  between them leaves `interrupted` and the client receives "unknown" (`UnknownReason::Interrupted`), never a
  re-execution.
- A non-200 response leaves the row `executing` without an owner until restart or acknowledgement (the client later
  gets "unrecorded" instead of the original rejection; S3).

### M-STORE-8 · Encrypted service receipts (`service-receipts.sqlite`)
- **Defined in**: `crates/daemon-server/src/service.rs:1190-1325, 2925-3020`.
- **States**: `accepted → executing → completed`; `executing → interrupted` (startup); `interrupted` with the same
  digest → back to `executing` but answered "uncertain" (409) except reads; `retired`. Concurrent duplicates are
  filtered by the `inflight` map.

### M-STORE-9 · Opening and migrating `domain.db`
- **Defined in**: `crates/store-sqlite/src/lib.rs:605-759` (`open`), `lifecycle_migration.rs` (schema 4→5, all in one
  transaction with a prior copy), `domain_database_migration.rs` (rename `domain-v2.db`→`domain.db`, marker
  `database-migration.json` Prepared/Committed with digests and folder fsync).
- **States**: `user_version` ∈ {0 (new), PREVIOUS (migrates), SCHEMA (opens)}; anything else → rejects, preserving the
  database. Schema migration and `user_version` in the same transaction: there is no half migration.
- **Restart recovery included in every `open`** (not only at daemon start): `delivery_attempts/domain_outbox`
  `dispatching→uncertain`, `schedule_runs launching→interrupted`, `launch.state dispatching→uncertain`
  (`lib.rs:724-735`).

### M-STORE-10 · Startup sequence (composite)
`crates/alondrad/src/lib.rs:121-158` and `crates/alondrad/src/assemble.rs:180-300`:
1. `home_renames::apply`, `refuse_legacy_home`, `migrate_and_fence` (exclusive lock only if there is a marker or a
   cutover), `retire_empty_older_sidecar`;
2. `HomeLock::shared` (shared: it does not exclude another daemon) and `ensure_ready`;
3. `wiring::open` → `DomainStore::open` (recovery of M-STORE-9) + `recover_settings` + `fill_task_*` + `load` (replay)
   + `capabilities::load` + commands `ReviewWaits` and `AcknowledgeRequestTasks`;
4. `DomainDaemon::over` (`Auth::open`);
5. `assemble`: `service.recover_interrupted()` and `action_results::recover_interrupted` (`assemble.rs:187-190`),
   **before** `TcpListener::bind` (`assemble.rs:259`); SDK adoption (`prepare_workers`) after the bind.
- Only processes supervised by launchd wait for the home folder to become free (`lib.rs:89-91`,
  `relaunch::wait_until_free`).

### M-STORE-11 · Append-only step journals
- `provider_transition_steps` (`crates/store-sqlite/src/provider_transition.rs:9-75`): phases
  `requested → draining → source_fenced → context_ready → target_admitted → target_ready → consumed → done`, with
  shortcuts, `aborted`/`uncertain` from any non-terminal, and **`done → start_refused → done`** (a terminal that
  reopens, only with `deferred_start`). One open per task (trigger).
- `account_move_steps` (`account_move.rs`): planning is disabled (`bail!`), only old moves are read.
- `sdk_handover_steps` (`handover.rs`): `planned → bg_stopped → sdk_admitted → done`, order by trigger.

### M-STORE-12 · State in JSON files in the home folder (implicit machines split with SQLite)
Inventory (helpers: `foundation/src/save.rs:9` `save::atomic` does fsync of the file, not of the folder;
`durability::flush` is used by the hand-written writers):

| File | Write | Also in SQLite? | Reconciliation at startup |
|---|---|---|---|
| `closed-sessions.json`, `closed-task-nodes.json`, `aborted-replaces.json` (`daemon-engine/src/live.rs:22-29`) | tmp+rename **without fsync**; an error is only logged | Yes: written **after** `ReleaseTask`/`RecordNodeEnd` (`daemon-engine/src/node_exit.rs:66-78`, `engine-sdk/src/task_closer.rs:264-278`) | None; unreadable → empty silently (`live.rs:107-113`) |
| `orchestrator.json` (`engine-orchestrator/src/domain_orchestrator.rs:125-131`) | tmp+rename without fsync | Yes, after the batch claim (`:509` → `:528`) | Yes, `repair_incomplete` (`domain_orchestrator/recovery.rs:26`); unreadable → retries every 5 s |
| `admission.json` (`engine-admission/src/admission.rs:304-310`) | `save::atomic` | Partial (pauses in SQLite first) | `pauses.rs:111-133` prunes `owed/resumed` |
| `sdk/<instance>/launch-options.json` (`daemon-engine-model/src/launch.rs:136`) | `save::atomic` | Partial (`selection`), JSON before the commit (`engine-sdk/src/provider_transition.rs:472-473`) | Not confirmed |
| `domain-capabilities.json` (`engine-kernel/src/domain_runtime/capabilities.rs:93-127`) | atomic with folder fsync | Derived; pruned against SQLite on load | Yes; unreadable → **does not start** |
| `options.toml` + `options.pending` (`engine-kernel/src/settings_projection.rs`) | two phases with a receipt in SQLite | Yes | Yes, `provider_settings::recover_store` |
| `options.toml` via `set_node_limit` (`engine-admission/src/admission.rs:1116`) | tmp+rename without fsync, without looking at `options.pending` | No | — |
| `database-migration.json`, `service-recovery.json` | atomic with folder fsync | — | Yes |
| `config.toml` | tmp+rename without fsync | No | unreadable → does not start |
| `startup-failures.json`, `auto-update.json` (in-place write), several device files | without fsync | No | by default |

### M-STORE-13 · Retention
`crates/store-sqlite/src/retention.rs` (bodies and receipts 7 days, except receipts pinned by batches; message texts of
finished tasks) and `crates/store/src/retention.rs` (observation bytes, commands of non-current executions, texts of
inputs of finished tasks except `uncertain` dispatches). Each pass in its own transaction; none of it is domain state
(the cache is kept).

### M-STORE-14 · Relaxed durability (`alondra_foundation::durability`, PR #493)
`crates/foundation/src/durability.rs`: `relax()` does `assert!(under_test())` (`BAZEL_TEST` present,
`foundation/src/lib.rs:21-24`). Callers: `engine-sdk/src/engine_tests.rs:27`
(`#[cfg(all(test, feature="whole-engine"))]`, `engine-sdk/src/lib.rs:50`), `daemon-server/src/engine_tests.rs:35`
(`#[cfg(test)]`, `daemon-server/src/lib.rs:118`), `alondrad/src/lib.rs:641` (inside `#[cfg(test)] mod engine`),
`alondrad/tests/codex_adoption.rs:29` (test binary). Effect: `synchronous=OFF` (`store-sqlite/src/connection.rs:66`)
and `flush` without `sync_all`. **Confirmed: it cannot be switched on outside tests** (no caller compiles in
production and the assertion requires `BAZEL_TEST`). `save::atomic` uses its own criterion (`BAZEL_TEST`), not this
switch.

### Other
- `alondra state audit/repair` (`crates/cli/src/state.rs`) only handles the legacy event log `alondra.db`; it does not
  audit or repair `domain.db`. The only audit of the current store is `alondrad --check-database`.

### Findings

#### F-STORE-1 · A second process applies restart recovery to the live database
**Class**: CONFIRMED · **Severity**: S2 · **Probability**: low · **Overlap**: none found in the `git log` of those
files. No task in the related-coverage notes and no issue in the open-issues list deals with exclusion between daemon
processes. the tests-by-layer work (host model) (point 30, `binary_smoke_test` with `--preflight`) runs the real binary in
tests, but not a second process over a live home folder. New evidence.
- **Invalid state/transition and broken invariant**: an in-flight delivery attempt of the live daemon goes to
  `uncertain` (and `schedule_runs` in `launching` to `interrupted`, local actions and service receipts in `executing`
  to `interrupted`) without any daemon having restarted. Broken invariant: "`uncertain/interrupted` by restart is
  only written by the process that succeeds the one that died".
- **Evidence**:
  1. `alondrad --test-service-push <device>` (documented in `--help`, `alondrad/src/lib.rs:47`) and a hand-started
     `alondrad` do not wait for the home folder to become free: `wait_until_free` only if `supervised`
     (`alondrad/src/lib.rs:89-91`).
  2. `migrate_and_fence` without `database-migration.json` or cutover returns before taking the exclusive lock
     (`store-sqlite/src/domain_database_migration.rs:118-127`); that is the case of every new installation.
  3. `HomeLock::shared` coexists with the live daemon's shared lock (`core/src/home_lock.rs:22-49`,
     `try_lock_shared`), `alondrad/src/lib.rs:145`.
  4. `wiring::open` → `DomainRuntime::open_observed` → `files.open` → `DomainStore::open`
     (`engine-kernel/src/domain_runtime.rs:164`), which unconditionally runs
     `UPDATE delivery_attempts SET state='uncertain'… WHERE state='dispatching'`, the outbox one and those of
     `schedule_runs` (`store-sqlite/src/lib.rs:724-735`), and also commits `ReviewWaits`/`AcknowledgeRequestTasks`
     (`domain_runtime.rs:216-217`).
  5. For a hand-started daemon, `assemble` also runs `service.recover_interrupted()` and
     `action_results::recover_interrupted` (`alondrad/src/assemble.rs:187-190`) before failing at
     `TcpListener::bind` (`assemble.rs:259`).
  6. In the live daemon, `finish_message` requires `state='dispatching' AND version=1` (`store-sqlite/src/lib.rs:1768`)
     → `Ok(false)` → `ensure!(finished)` (`daemon-server/src/domain_delivery.rs:100`) → the worker returns `Err`
     (`:51`) → `supervise_workers` ends the daemon (`alondrad/src/lib.rs:238-243`). The completed local action gets
     `changed==0` → "unrecorded" (`views/action_results.rs:320-321, 339-341`).
- **Minimal sequence**: (1) the daemon claims a push (`dispatching` v1) and waits for APNs; (2) the user runs
  `alondrad --test-service-push phone-1`; (3) that process opens `domain.db` and leaves the attempt in `uncertain` v2;
  (4) APNs answers, `finish_message` returns false, the daemon exits and launchd relaunches it. The push went out but
  is left as uncertain; a scheduled launch in progress is left `interrupted` although it started.
- **Impact**: daemon crash, false delivery evidence, a scheduled launch marked as interrupted (possible duplicated
  manual relaunch), an "unknown" answer for an action that was done.
- **Deterministic test and ablation**: a store test in `store-sqlite` with `test_support::Directory`: open
  `DomainStore` A, seed a message, `claim_message`; open `DomainStore` B on the same file; `A.finish_message(claim,
  Handed)` must return `true`. Today it returns `false`. For the full path: a test in `alondrad` that takes
  `HomeLock::shared`, calls `wiring::open` twice and checks the attempt state. Ablation: remove the proposed guard
  and see it fail.
- **Expected fix and files**: an exclusive writer lock for the daemon (for example the exclusive lock of a
  `daemon.lock` file, or `BEGIN EXCLUSIVE` on a sentinel file) taken before `DomainStore::open`; make
  `--test-service-push` talk to the live daemon or open read-only; move restart recovery out of `DomainStore::open`
  into an explicit step that only the lock owner performs. Files: `alondrad/src/lib.rs`, `core/src/home_lock.rs`,
  `store-sqlite/src/lib.rs:724-735`.

#### F-STORE-2 · `alondrad --preflight` does not exercise the JSON files the real startup requires
**Class**: CONFIRMED (a coverage gap; whether it fires depends on a future format change) · **Severity**: S2 ·
**Probability**: low · **Overlap**: the tests-by-layer work (host model), point 30 (`//crates/alondrad:binary_smoke_test`
with `--preflight`), tests that the binary starts its preflight, but not what the snapshot copies. New evidence for
that task: its test could include a `domain-capabilities.json` of another format. No open issue.
- **Invalid state/transition and broken invariant**: a binary judged "fit" by preflight that, when it replaces the
  daemon, does not start in the real home folder.
- **Evidence**:
  1. `snapshot` copies only the databases in `DATABASES` and two markers (`daemon-server/src/preflight.rs:12-18,
     60-96`).
  2. The real startup fails with an unreadable `domain-capabilities.json` or one of another format
     (`engine-kernel/src/domain_runtime/capabilities.rs:46-72`, `deny_unknown_fields`, `format == 1`) and with an
     unreadable `devices.json` (`daemon-support/src/auth.rs`, `Auth::open` from `DomainDaemon::over_shared`,
     `daemon-server/src/domain_bootstrap.rs:80`).
  3. In the copy those files are missing and defaults are taken, so preflight passes (`preflight.rs:48-57`).
- **Minimal sequence**: (1) a version changes the format of `Principal` or of `devices.json`; (2) preflight on the
  real home folder passes; (3) the relaunch starts the new binary, `capabilities::load` fails and it enters the
  failed-starts loop that preflight was meant to avoid.
- **Impact**: a bad binary replaces the daemon and cannot start.
- **Deterministic test and ablation**: a `preflight.rs` test with a home folder that has a `domain-capabilities.json`
  with `format: 2` (or invalid JSON): today `run_for` returns `Ok`; it should fail like `DomainRuntime::open`.
  Ablation: remove the file copy.
- **Expected fix and files**: copy to the snapshot (with the same ownership and mode checks) all the state files the
  startup reads (`domain-capabilities.json`, `devices.json`, `options.toml`/`options.pending`, `config.toml`,
  `service.toml`/`service-recovery.json`, `orchestrator.json`, `admission.json`), or validate their reading in the
  real home folder read-only. File: `daemon-server/src/preflight.rs`.

#### F-STORE-3 · "Retry" of an uncertain orchestrator message older than 24 h discards itself
**Class**: CONFIRMED · **Severity**: S3 · **Probability**: medium (nobody checks `alondra delivery list` daily) ·
**Overlap**: orchestrator area (TTL of its inbox); new evidence on the interaction with `alondra delivery`.
the stalled-node recovery work (phase 4) changes the `ANSWER` deadline of the orchestrator inbox, not the 24 h expiry of the
outbox. No open issue.
- **Invalid state/transition and broken invariant**: `uncertain → retryable` (explicit user decision) → `discarded`
  in ≤10 s without notice.
- **Evidence**: `alondra delivery retry` requires `--checked --confirm-duplicate-risk` (`cli/src/delivery.rs:33-42`)
  and the answer says `state: retryable` (`store-sqlite/src/lib.rs:1840-1849`); the outbox loop discards every 10 s
  the orchestrator's `pending/retryable` with `command_audit.at` older than 24 h
  (`daemon-server/src/domain_delivery.rs:37-41`, `store-sqlite/src/lib.rs:1625-1639`), and `claim_in` does the same
  (`lib.rs:970-979`). The age is measured from the original commit, not from the decision.
- **Impact**: the user's explicit retry decision is silently undone.
- **Deterministic test and ablation**: a store test: orchestrator message with
  `set_message_created_at_for_test(now−25 h)`, claim, finish `Uncertain`, `resolve_uncertain(retry=true)`, then
  `discard_expired_orchestrator_messages(now−24 h)` → today 1 change (discarded). Ablation: count the age from the
  reconciliation.
- **Expected fix and files**: at least, reject the retry with a clear error if it already expired; or count the age
  from the user's reconciliation.

#### F-STORE-4 · A released task whose `closed-task-nodes.json` entry is lost is left without a node forever: nothing relaunches it or says so
**Class**: CONFIRMED (it was a hypothesis). Every path that relaunches a released task depends only on that entry, and
none rebuilds it or detects its absence. What depends on a failure is how the entry gets lost (three ways, below). ·
**Severity**: S1: the task is stuck with no way out in the product (no button, no card and no automatic relaunch). ·
**Probability**: low · **Overlap**: issue #323 ("Guarantee an outcome for every dependent on cancel or replace")
covers replacement, which uses `take_out` with `ByItself`. This evidence is new for it: the replaced task can be left
without an outcome if the entry is lost. No task in the related-coverage notes covers it; the stalled-node recovery work
deals with nodes that go silent, not with tasks without a node. Same class of JSON/SQLite split state as F-ENGINE-3.
The `git log` of `live.rs` shows only crate moves.
- **Invalid state/transition and broken invariant**: an open task in `NotStarted`, with no agent and with a launch
  recorded, with no entry in `closed-task-nodes.json`. Broken invariant: "every open task is in someone's hands". The
  domain itself declares it `Ownerless` ("its node is gone and nobody was asked", `domain-rules/src/ownership.rs:302`),
  but that check only runs in tests (`ownership_tests.rs:468`): no production code calls `holder()`.
- **Who writes the entry, and in what order**:
  - `node_exit::take_out` (`daemon-engine/src/node_exit.rs:52-86`) commits `ReleaseTask` (`:62-67`) and then calls
    `remember_closed_task` (`:70`). It is used by the user's close (`user_closed`, from `node_close.rs:72, 128`, with
    `Relaunch::AskUser`), the orchestrator's replacement (`domain_orchestrator_tools.rs:678, 685`) and the aborted
    replacement (`node_exit.rs:42`), the last two with `ByItself`.
  - `task_closer::record_end` (`engine-sdk/src/task_closer.rs:259-282`) commits `RecordNodeEnd`. For a task that went
    back to work, the rule releases it and deletes `wrap_up` (`domain-rules/src/rules.rs:5380-5386`). Then it calls
    `remember_closed_task` with `ByItself` (`:278`).
  - `remember_closed_task` changes the in-memory map and calls `save` (`daemon-engine/src/live.rs:141-150`). `save` is
    write+rename without fsync, and a failure is only logged (`:22-30`).
  - At startup, `Live::persisted_in` reads the file; if it is missing or cannot be read, the map is left empty
    without notice (`live.rs:107-113`).
- **What the release leaves**: `ExecutionReleased` moves the lifecycle to `Queued`
  (`domain/src/lifecycle/commands.rs:1148-1168`), and `materialize` leaves `agent_id = None`, `started = false` and
  `state = NotStarted` (`domain-rules/src/lifecycle.rs:1314-1328`). `needs_a_node_again` is false, because it had an
  agent before and was not terminal (`store-sqlite/src/command.rs:456-460`): it does not go back to `launch_queue`.
- **Every exit requires the entry**:
  1. `auto_launch::pass`: without the entry, `relaunching = false`; `task_was_launched` is true and it does
     `continue` (`engine-sdk/src/auto_launch.rs:113-119`; `store-sqlite/src/lib.rs:1448-1453`).
  2. `relaunch_from`, used by the user's «Relanzar» (Relaunch) button (`daemon-engine/src/wiring.rs:268`) and the
     `start_retries` retry: without the entry it returns `conflict` "This task is not waiting for a relaunch"
     (`daemon-engine/src/domain_orchestrator_tools.rs:814-819`). `start_retries::retry` does not even try if `closed`
     is false (`engine-sdk/src/start_retries.rs:99-101`).
  3. The «Relanzar» button only appears with `node_closed`, which requires the entry (`views/src/tasks.rs:24-27`,
     `views/src/common.rs:221-224`).
  4. `start_failures::facts` gives `start = Launched` and `closed = false` (`daemon-engine/src/start_failures.rs:28-38`).
     With that, `DomainState::react` produces neither `Retry` nor `Ask(NodeClosed)`
     (`domain-rules/src/ownership.rs:358-380`): no card.
  5. No deadline rescues it: `obligation_watch` only enforces `HandInput`, `Wake` and `Answer`
     (`domain-rules/src/obligation_policy.rs:15-27`), and a queued task owes none of them.
  6. SDK retakes (`to_take`/`unowned`) require `agent_id` (`engine-sdk/src/sdk_owner.rs:1520-1521`).
  - Only manual exit: the user cancels the task, or asks the orchestrator to launch it. Its `launch_task`
    (`launch_guarded`, `domain_orchestrator_tools.rs:415-470`) does not look at the entry. Nothing suggests either.
- **Three ways to lose the entry**:
  - (a) The daemon dies between the commit and the `save` (`node_exit.rs:67→70`, `task_closer.rs:263→278`). On
    restart, nothing repeats it: `take_out` filters by `agent_id == agent`, which is already `None`. The
    `task_closer` retry sends `RecordNodeEnd` again, the domain rejects it with `conflict` ("The task has another node
    by now", `rules.rs:5367-5368`), and `record_end` returns `Ok(())` without `remember` (`task_closer.rs:264-271`).
    After that, `forget_task_closing` (`:299`).
  - (b) The `save` fails (full disk, which Alondra does experience; see the disk incidents of M-ENGINE-11) and the
    daemon restarts before another `remember`/`forget` rewrites the whole map from memory (`live.rs:141-150`,
    `:179-188`).
  - (c) The file cannot be read at startup (a broken write after a power cut, F-STORE-5): **all** entries are lost at
    once, without notice.
- **Minimal sequence** (variant b, deterministic with a write failure):
  1. The user closes T's node. `ReleaseTask` is committed.
  2. `save` of `closed-task-nodes.json` fails and is logged. Memory has the entry: the «Relanzar» button shows.
  3. The daemon restarts (update, `alondra update`) before another close.
  4. T appears as a task not started, with no «Relanzar» button or card, and no `auto_launch` pass launches it.
- **Impact**: the task is stuck with no way out inside the product.
- **Deterministic test and ablation**:
  - Pure domain: `DomainState::react` with a released task and `StartFacts { start: Launched, closed: false, node: Gone
    }` returns no reaction, while `holder()` gives `Ownerless`. It is the gap in one line.
  - Engine with `test_support::Directory`: `take_out` with `closed-task-nodes.next` created as a directory (the write
    fails), and then a new `DomainRuntime`/`Live` on the same folder. `auto_launch::pass` (with launch doubles) does
    not launch, `start_retries::react` creates no card and `relaunch_from` gives `conflict`. With the fix, the task
    is relaunched (`ByItself`) or the `NodeClosed` card appears (`AskUser`).
  - Ablation: read the entry from the JSON again instead of SQLite and see the test fail.
- **Expected fix and files**:
  - store the closed node (`name`, `cwd`, `relaunch`) in SQLite, in the same transaction as `ReleaseTask` and
    `RecordNodeEnd`, for example as command data or in a table written by the commit, and derive `closed_task` from
    there;
  - as a safety net, the daemon checks the `holder()` invariant in production and turns an `Ownerless` into a card or
    a relaunch, instead of only in tests;
  - files: `daemon-engine/src/node_exit.rs`, `engine-sdk/src/task_closer.rs`, `daemon-engine/src/live.rs`,
    `store-sqlite`, `domain-rules/src/ownership.rs`.

### Hypotheses

#### F-STORE-5 · State writers without fsync (rename lost after a power cut)
**Class**: HYPOTHESIS (depends on the file system after a power cut) · **Severity if confirmed**: S2-S3.
- **Files involved**: `orchestrator.json` (`domain_orchestrator.rs:125-131`), `closed-*.json` (`live.rs:22-29`),
  `config.toml` (`core/src/config.rs:396-397`), `startup-failures.json`; `auto-update.json` is written in place
  (`daemon-support/src/updates.rs:47`). `save::atomic` does fsync of the file but not of the folder
  (`foundation/src/save.rs:9-29`). For `orchestrator.json` there is reconciliation with SQLite; for `closed-*` there
  is not.
- **Missing to confirm**: the behaviour of APFS when an unsynced rename is lost; a test is only possible with a `Disk`
  double that loses renames without fsync.

#### F-STORE-6 · `set_node_limit` writes `options.toml` outside the two-phase settings protocol
**Class**: HYPOTHESIS · **Severity if confirmed**: S2 · **Overlap**: the Settings redesign (issues #329, #395, #396
and #397: accounts, limits and model assignment) is going to touch the writers of `options.toml`. New evidence for
them: the node limit is written outside the two-phase protocol. No task in the related-coverage notes.
- `engine-admission/src/admission.rs:1116` does tmp+rename without looking at `options.pending` or taking the
  settings writer's lock; `provider_settings::recover_store` refuses when `options.toml` changed outside the pending
  transaction (`store-sqlite/src/provider_settings.rs:64-65`), which would block the next settings read or change.
- **Missing to confirm**: whether the two writers can coincide (a settings change with `options.pending` alive while
  admission adjusts the limit) and what the user sees in that case.

## Pull requests, merges, worktrees, closing and retention (`GH`)

Scope: `crates/engine-github`, `crates/github-gh`, `crates/engine-worktrees`, `crates/system` (the `Worktrees` port),
the worktree and PR part of `crates/engine-sdk/src/task_closer.rs`, file retention
(`daemon-support/src/files_retention.rs`, `engine-kernel/src/retention.rs`) and PR #503. Retention of the store itself
(M-STORE-13) is not repeated here. Audited at commit `1567967b9`.

### M-GH-1 · Pull request state (`PullRequest`)
- **Kind**: explicit (enum `PrState {Open, Closed, Merged}`) plus the fields `head`, `base`, `merged_head`,
  `allowed_methods`, `review`. **Defined in**: `crates/domain-rules/src/model.rs:517-625`. Transitions in
  `rules.rs:5095-5180` (`ObservePrState`), `rules.rs:4597-4727` (`RegisterPr`, including the move between tasks) and
  `rules.rs:4728-4760` (`NotePrOpened`/`RecordOpenedPr`).
- **Source of truth**: GitHub. **Copies and derivatives**: `DomainState.pull_requests` (table `pull_requests`; owners in
  `pull_request_tasks`; history in `pr_task_history`); `Task.pr_id`/`later_prs`; the server's in-memory PR details cache
  (`daemon-server/src/client_routes/pr_details.rs`); the reads of `Merger` and of the watcher. Three writers of
  observations have no order between them: the watcher (`domain_github::reconcile`, every 60 s and whenever the watched
  set changes), the merger (`domain_merges::record_merged`/`observe_status`) and `catch_up_task` on delivery.
- **Composite states that matter**: PR x the task's `OpenPrPending` (M-GH-4) x authorization (M-GH-2) x task state.

| From | Event [guard] | To / effect |
|---|---|---|
| (none) | `RegisterPr` (assigned agent) [task is not a reviewer, canonical id, non-empty methods] | Open; `created` |
| (none) | `RecordOpenedPr` (system) [PR is in `opened_prs`] | whatever state GitHub reports |
| Open | `ObservePrState`, different head | Open with the new head; `invalidate(task)`: acceptances and authorizations become invalid |
| Open | `ObservePrState` Closed | Closed; if no open PR is left and a `Close` was pending, `open_pr=None`; if the delivery was "discarded", `cancel`; otherwise a notice to the node |
| Open/Closed | `ObservePrState` Merged [exact `merged_head`] | Merged; Claimed/Uncertain authorizations at the same head become Confirmed; notice to the node |
| Closed | `ObservePrState` Open | Open (a closed PR can be reopened) |
| Merged | `ObservePrState` not Merged, or different head | **rejected** `invalid_input` ("Confirmed merge cannot be undone") |
| PR of a terminal task | `RegisterPr` from another task | move: new owner, `state=Open` and `merged_head=None` **without looking at the previous state**, `old.open_pr=None` |
| PR of a live task | `RegisterPr` from another task | **rejected** `pr_owned_by_other_task` |

- **Terminals**: Merged is terminal by a guard in `ObservePrState`, but `RegisterPr` from another task rewrites it to Open
  (`rules.rs:4668-4672`). This overlaps the stable pull request identity work and is not developed here (see H-GH-2).
- **Invariants**: `state==Merged <=> merged_head.is_some()` (checked in `ObservePrState`); a PR has one live owner
  (checked in `RegisterPr`/`RecordOpenedPr`). **Assumed, not enforced**: that observations arrive in order. There is no
  version token, so an old read applied late invalidates the acceptance (see H-GH-1).
- **Persistence and restart**: everything lives in SQLite through domain commands. Re-applying an identical observation is
  a no-op (`apply` compares before writing, `domain_github.rs:315-318`).
- **Concurrency**: commands are serialized in the domain with `expected_revision` read just before
  (`execute_system`, `domain_github.rs:19-28`). A revision conflict is logged and retried on the next round. Decisions
  are taken on snapshots that can be seconds old (see F-GH-1).

### M-GH-2 · Merge attempt (`MergeAuthorization`)
- **Kind**: explicit. **Defined in**: `model.rs:1408-1413`: `MergeAttemptState {Unclaimed, Claimed, Uncertain, Confirmed,
  Failed}` + `valid: bool` + `claimed_by: Option`. Transitions: `AuthorizeMerge`/`AcceptDelivery` (`rules.rs:4987-5030`),
  `ClaimMergeAuthorization` (`rules.rs:5031-5074`), `ObserveMergeAttempt` (`rules.rs:5075-5094`), `ObservePrState`
  (Confirmed), `invalidate`/`invalidate_feedback` (`rules.rs:1646-1726`, set `valid=false`).
- **Source of truth**: the authorizations table in `domain.db`. **Copies and derivatives**: `Merger.pressed` (memory).

| From | Event [guard] | To |
|---|---|---|
| none | `AcceptDelivery` (user) / `AuthorizeMerge` (user, or system for auto-merge) | Unclaimed, valid |
| Unclaimed, or Claimed/Uncertain with `claimed_by` | `ClaimMergeAuthorization` (system only) [task active, no pending answer, valid, PR Open and local head = remote head = authorized head, base verified and equal, `stack_ready_for_merge`] | Claimed, `claimed_by=None` |
| Claimed without `claimed_by` | `ClaimMergeAuthorization` | **rejected** `delivery_uncertain` |
| Claimed/Uncertain | `ObserveMergeAttempt` Uncertain/Failed | that state |
| Claimed/Uncertain | `ObservePrState` Merged at the authorized head | Confirmed |
| Claimed/Uncertain | watcher sees Closed, Merged at another head, or Open with another head | Failed (`domain_github.rs:337-357`) |
| any | new head, feedback on the PR, `ReplacePresentation`, ... | `valid=false` (the attempt state does not change) |

- **Terminals**: Confirmed and Failed. Nothing reopens them; a new button press creates another authorization.
- **Uncertain**: no production code writes it today (only the guards accept it). It is a dead state.
- **Persistence and restart**: Claimed survives. After a restart `Merger::step` does not claim again and calls
  `gh pr merge` again (`domain_merges.rs:419-427`). This is idempotent because GitHub answers "already merged" or the PR
  is in the queue.

### M-GH-3 · Merge loop (`domain_merges::Merger`)
- **Kind**: implicit, in memory. **Defined in**: `domain_merges.rs`. `due(state)` (`:38-74`) decides which PR is next. Per
  PR it keeps `waited`, `transient` (retries 30 s x 2^n, 6 attempts), `trouble`/`merge_trouble` (plus a problem card),
  `queued` (GitHub's merge queue), `action` (the "act on the checks" card), `pressed` and `bypass_refused`
  (`domain_merges.rs:127-150`).
- **Source of truth**: the domain for "what is due"; GitHub for the real state; everything else is lost on restart.
- **Events and steps**: every domain commit wakes the loop (`domain.subscribe()`), or the pause (`poll`=60 s, or the
  nearest retry). Steps of `step` (`domain_merges.rs:291-457`): read state; Merged: record; Closed or Open with another
  head: `Settled` **without recording anything**; conflict or failed checks: `hand_back` (system `Conflicts` feedback);
  checks that ask for action: card; pending/required checks: wait, or skip them if the user authorized and GitHub allows
  skipping; authorize (auto-merge) and claim; `still_due` right before `ready` and before `merge`; `merge` or
  `merge_bypassing`; `confirm` (Merged: record; otherwise `queued`).
- **Rejected**: a command the domain rejects is logged and the step stays in `Wait` (`recorded`, `:658-669`).
- **Concurrency**: one loop only (the `Merger` in `views/actions.rs:3981` is test-only). `still_due` is evaluated with no
  `await` between it and `gh pr merge`; a cancellation that arrives during the `gh` call cannot be avoided.
- **Persistence and restart**: `queued` is lost. A PR already in GitHub's merge queue goes through checks and
  `gh pr merge` again (which GitHub resolves). The retry counters are lost (6 attempts again).

### M-GH-4 · Open PR of a closed task (`OpenPrPending`, table `task_open_prs`)
- **Kind**: explicit. **Defined in**: `model.rs:263-272` (`Option<OpenPrPending>` with `Deliver{result}` /
  `Close{comment}`); rules in `rules/open_pr.rs`. Effectors in `domain_github.rs:117-201` (`reconcile`, `deliver_held`,
  `settle_status`) and their in-memory bookkeeping `ObservationFailures` (failed reads, rejected closes with doubled
  wait, cards).
- **States**: None -> `Close` (user cancellation with an open PR, `close_task`) / `Deliver` (closed by the orchestrator or
  Alondra, or `AdoptOpenPr`). `Close` -> None when the PR stops being open (`ObservePrState`, `rules.rs:5138-5142`),
  when another task takes the PR (`RegisterPr`, `rules.rs:4664`) or on reopen (`ReopenTask`, `rules.rs:5300`).
  `Deliver` -> None with `SettleOpenPr` (Closed: `cancel`; Open/Merged: `OpenPrDelivery` delivery card).
- **Effects**: `observer.close(...)` (comment plus close on GitHub) from `reconcile` when the PR is still Open and the
  snapshot says `Close` (`domain_github.rs:131-146`). See F-GH-1.
- **Persistence and restart**: the pending value lives in SQLite; the failure counters and the doubled wait are lost
  (the attempt is repeated).

### M-GH-5 · PRs seen opening and not yet read (`Task.opened_prs`)
- **Kind**: implicit. A set on the task (column `opened_prs` of `task_relations`). `NotePrOpened` adds a PR if it is not
  registered and the task is not terminal; `RecordOpenedPr` removes it and registers/observes it; `RegisterPr` removes
  it. Read on every watcher round (`read_opened`) and on delivery (`catch_up_task`). A PR with no known merge methods
  stays waiting (the log says so; delivery waits).

### M-GH-6 · Stacked PRs (`Task.stack`, table `task_stacks`, `stack_readiness`)
- **Kind**: composite. `Task.stack {upstream_task, presentation_id, pr_id, head}` + `StackReadiness{ready}` published by
  the upstream node (`PublishStackReadiness`) + the state of the upstream PR. `stack_ready` (start/due) and
  `stack_ready_for_merge` (claim/merge: upstream PR Merged at that head and equal bases) in `rules.rs:2034-2070`.
- The merger retargets the lower PR to the default branch when the upstream one is merged (`retarget_stack`,
  `retarget_sibling`, `move_onto`, `domain_merges.rs:208-289`) and rereads the state before merging a stacked PR.
- `invalidate_feedback` sets `ready=false`. No new findings: the guards reread GitHub and the domain at every step.

### M-GH-7 · Hourly sweep of PRs without an owner (`open_pr_sweep`)
- **Kind**: implicit, no state of its own. Every hour (first run after 5 minutes) it looks for the user's open PRs in
  repos of the projects folder. For each PR registered by a terminal task with no pending `Close` and whose branch does
  not belong to a live task (`worktree-<id>`): `ClosePrOfCancelledTask` if the task is cancelled, otherwise
  `AdoptOpenPr` (reopens the task and its delivery) (`open_pr_sweep.rs:68-96`). The domain revalidates both
  (`rules/open_pr.rs:66-103`, `rules.rs:5259-5296`).

### M-GH-8 · Worktree record of a task (`task_worktree_records`, `task_worktrees`, `worktree_storage`)
- **Kind**: explicit per worktree. **Defined in**: `model.rs:312-345` (`TaskWorktree {origin: Task|Inherited|Subagent|Entered,
  branch, discard, end: None|Removed|KeptByUser|UsersOwn|HandedOver}`), CHECK in `store-sqlite/src/lib.rs:284-291`.
  Commands `RecordTaskWorktree`, `ReleaseTaskWorktree`, `ConfirmCleanup{discard}`.
- **Source of truth**: the disk and `git worktree list`. **Copies and derivatives**: the domain record, and
  **`task_worktrees` (task -> launch folder), which is never deleted** (there is no `DELETE FROM task_worktrees` in the
  repo) and is used to relaunch in the same folder and in the sweeps of cancelled tasks. The two can diverge: the record
  says `HandedOver` or `Removed` while `task_worktrees` still names the folder (see F-GH-3).
- **Events**: launch (`set_task_worktree` + `RecordTaskWorktree Task`, `domain_runtime.rs:806-810` and
  `store-sqlite/owner.rs:723-731`), `PostToolUse` hook of EnterWorktree/isolated subagent (`task_worktrees.rs:57-90`),
  recovery of the provider's notes on close (`recover_noted`, `:92-115`), inheritance (`heir`, `:120-143`).
- **Terminals**: `end` does not go back to None except through a new `RecordTaskWorktree`.
- **Invariants**: assumed: "a worktree with `end=None` exists and belongs to a task". Only `release_all` / the close
  enforces it.

### M-GH-9 · Deleting a worktree on disk and its branch (`crates/system/src/worktree.rs`)
- **Kind**: implicit in the file system. Folder -> `.<task>.removing` next to it (same volume) -> deleted. A cut-short
  delete resumes from the trash (`release`, `worktree.rs:316-345`). For links from `.claude/worktrees` to another volume,
  the target is deleted only if it is a worktree of the same repo (`release_linked`).
- **Guards for "unsaved work"**: `unsaved` = uncommitted files (`status --porcelain --untracked-files=all`) + HEAD commits
  that no remote has, **except** if the repo has no remote (`LocalHome::NoRemote`, counts 0) or HEAD is already in the
  local default branch (`worktree.rs:83-125`, `:629-647`). Files ignored by git do not count.
- **Branch (PR #503)**: after `Removed`, `release_finished_worktree` deletes `worktree-<folder>` with `git branch -D` if
  the task has **any** merged PR (`merged_pr(task)`, `rules.rs:1950`) or if the branch is already in the local default
  branch (`worktree.rs:671-701`). `discard_worktree` (cancelled tasks) deletes it only if it is on a remote or in the
  default branch (`delete_saved_branch`, `:703-716`). See F-GH-4.
- **Callers**: `release_all` (`task_worktrees.rs:177-225`), `finish_unattended` and `finish_with_worktree`
  (`task_closer.rs:176-185`, `:588-603`), `release_cancelled`/`apply_answers`/`sweep` (cancelled tasks), `sweep_shells`.

### M-GH-10 · Worktrees of cancelled tasks and their card (`cancelled_worktrees.rs`)
- **Kind**: composite. A worktree x the system card `DiscardWorktrees` (question + form; open, answered "delete all" /
  "delete the marked" / "keep", or withdrawn).
- `release_cancelled` (when a cancelled task closes): no uncommitted files -> `discard_worktree` (Removed); with them ->
  `KeptByUser` and one card per task (`:66-107`). `apply_answers` (at the start of **every** closer pass,
  `task_closer.rs:222-224`) deletes with `discard_worktree` (without looking at unsaved work) every path that an answered
  card asked to delete and that is "not in use" (`:121-142`). `sweep` (at daemon start) goes over the cancelled tasks
  (`:146-180`).
- **Terminals**: an answered card is never withdrawn, and compaction keeps Alondra's cards (`compaction.rs:7`), so its
  effect is reapplied forever. See F-GH-2.

### M-GH-11 · Task close: domain wrap-up + closer bookkeeping
- **Domain** (`task_wrap_ups`): `asked_at`, `cleaned_at`, `close_cancelled`, `node_end: Closed|Gone|KeptByUser|UsersOwn`
  (`model.rs:425-458`). `wrap_up_step` asks for cleanup when the delivery settles (`rules.rs:2549-2575`); `ConfirmCleanup`
  (`rules.rs:5317-5344`); `RecordNodeEnd` (`rules.rs:5364-5420`, requires that no worktrees are left open for Closed/Gone).
- **Closer** (`task_closings`, not domain state): `since`, `failures`, `last_error`, `next_attempt`,
  `attempted_cleaned_at`, `rm_started_at`, `ready_since`, `problem_id` (`store-sqlite/src/lib.rs:224-234`). A pass runs
  every 30 s or on every commit (`task_closer.rs:55-108`). `Due::{Cleanup, Cancelled, Unattended}` (`:110-128`). Three
  failures raise a `CloseProblem` card (Retry / Leave, which gives `KeptByUser`).
- **Closing order**: check unsaved work (`leftovers`, except for cancelled tasks) -> close the node -> `release_all` and
  the launch folder -> `RecordNodeEnd` -> delete the task scratch (`files/tasks/<id>`) -> forget the row. Before deleting,
  a copy of the state is used to test that the domain will accept the node end (`refused_end`, `:610-622`).
- **Persistence and restart**: the `task_closings` row survives; `rm_started_at` marks a cut-short `claude rm`;
  `claim_task_removal` prevents two closes at once.

### M-GH-12 · Work after the merge (`task_post_merge_work`, `task_post_merge_turns`, obligation `PostMerge`)
- **Kind**: composite. `WrapUp.post_merge_work` + the lifecycle obligation `DrainObligation::PostMerge`
  (`domain/src/lifecycle/commands.rs:384-403`) + the inherited `post_merge_checks` (only read to withdraw its card).
- Declared with `DeclarePostMergeWork{reason}`. It ends **only** with a fact: `DeclarePostMergeWork` without a reason,
  `ConfirmCleanup`, end of session (`RecordNodeEnd Gone`, or the host ending, which gives a resumed session or a card to
  the user, `observations.rs:681-701`), or the user's decision on the `PostMergeProblem` card. There is no deadline
  (`task_closer.rs:4-6`, a documented decision). While it is owed, the task is Working ("after merge") and **holds a gate
  slot** (`holds_slot`, `domain/src/lifecycle.rs:1429-1440`). See F-GH-5.

### M-GH-13 · Retention of the files folder (`files_retention.rs`; since PR #503 it also deletes)
- **Kind**: implicit, per folder. `tasks/<id>` (goes when the task is terminal; also in the closer's `finish`),
  `results/<id>` (14 days after close if its card is not in the inbox), `uploads|images|domain-v2|documents/<x>` (30 days
  from last modification if no open card, card of an open task, or document names them), and **any other top-level
  entry** ("without owner", 30 days). Hourly pass in `engine-kernel/src/retention.rs`.
- **Source of ownership**: `References::of` scans only `state.interactions` and `document_revisions`
  (`files_retention.rs:205-220`). It does not look at requests, task entries or the PR image cache. See F-GH-6.
- There is no lock with whoever writes in those folders; it decides from a state snapshot and mtime.

### Findings

#### F-GH-1 · The watcher closes on GitHub the PR of a task the user just reopened (or that another task just took)
**Class**: CONFIRMED · **Severity**: S2 · **Probability**: low-medium · **Overlap**: none in the plans of related work; the stable pull request identity work covers PR moves, not this race.
- **Invalid state/transition and broken invariant**: a live (reopened) task, or a PR with a new live owner, and the PR
  closed by Alondra with the comment "Cancelled in Alondra". It breaks "Alondra only closes the PR of a task the user
  cancelled and that is still cancelled" (`rules/open_pr.rs:1-5`).
- **Evidence**:
  1. `reconcile` takes a snapshot once (`domain_github.rs:121`) and walks all watched PRs with one network read per PR
     (`:122-123`, `observe(...).await`).
  2. The decision to close uses that snapshot: `pending(&state, &pr)` (`:131-133`), not the current state.
  3. Meanwhile `ReopenTask` sets `open_pr = None` (`rules.rs:5300`) and `RegisterPr` from another task does
     `old.open_pr = None` and changes the owner (`rules.rs:4660-4672`).
  4. `observer.close(...)` closes the PR (`domain_github.rs:135`); the next observation applies Closed to the PR, which now
     belongs to the live task (`ObservePrState`, `rules.rs:5131-5178`): a "PR closed" notice to the node.
- **Minimal sequence**: (1) The user cancels T with its PR #7 open -> `open_pr = Close`. (2) The commit wakes the watcher:
  the `pending` set grew (`domain_github.rs:398-401`) and `reconcile` starts with snapshot S0. (3) While it reads the PRs
  before #7, the user presses "Reopen" (or the orchestrator calls `reopen_task`). (4) On reaching #7, `pending(S0)` still
  says `Close` -> `gh pr close`.
  Interleaving: the watcher thread between `domain.state()` (`:121`) and `observer.close` (`:135`) for that PR. With N
  watched PRs the window is the sum of N reads (`gh pr view` + repo methods, up to 30 s each).
- **Impact**: the PR of a task that is still working appears closed on GitHub with "Cancelled in Alondra"; the node gets
  "PR closed" and has to reopen it; if nobody notices, the next delivery is presented on a closed PR.
- **Deterministic test and ablation**: engine with doubles (`domain_github` tests): a `FakeObserver` with a hook in
  `observe` of PR #1 that runs `ReopenTask` of T (or `RegisterPr` of T2 on #7); T cancelled by the user with #7 open.
  Today `FakeObserver.1` contains the close of #7. Expected: no close. Ablation: go back to using `pending(&state, ...)`.
- **Expected fix and files**: reread `pending(&*domain.state()?, &pr)` (and check that `pr.task_id` is still the owner)
  right before `observer.close`; better still, make the close a domain effect with its receipt (like
  `ClosePrOfCancelledTask`). File: `crates/engine-github/src/domain_github.rs`.

#### F-GH-2 · An already-answered "delete unsaved changes?" card deletes the recreated worktree of a reopened task, on every closer pass
**Class**: CONFIRMED · **Severity**: S1 · **Probability**: low · **Overlap**: none.
- **Invalid state/transition and broken invariant**: a live task whose worktree Alondra deletes without checking for unsaved
  work. It breaks "a worktree with unsaved work is not touched without the user's word" (`cancelled_worktrees.rs:1-4`):
  the user's word was about another life of the task.
- **Evidence**:
  1. Closing a cancelled task with uncommitted files: `KeptByUser` + a `DiscardWorktrees` card with path P
     (`cancelled_worktrees.rs:78-104`).
  2. `apply_answers` runs at the start of every closer pass (`task_closer.rs:221-224`, every 30 s or on every commit) and
     goes over **all** answered, non-withdrawn cards (`cancelled_worktrees.rs:109-113`, `:121-124`). Nobody withdraws the
     card and compaction keeps Alondra's cards (`domain-rules/src/compaction.rs:7`).
  3. It does not check whether the task is still cancelled; only `in_use`, which **excludes the task itself**
     (`task_worktrees.rs:257-261`: `worktree_held_by_other(path, task)` and `other.id != task`).
  4. It deletes with `discard_worktree` = `release(dir, |_| true, give_up=true, ...)` (`system/src/worktree.rs:658-668`):
     no unsaved-work check; it also deletes the branch if it is on a remote or in the default branch.
  5. Reopening a cancelled task is allowed (`reopen_guard`, `guards.rs:434-455`: terminal + open request; "Reopen" button in
     `views/actions.rs:1604-1607` and the `reopen_task` tool), and the relaunch recreates the worktree in the same folder
     (`reopen(cwd, slug, original...)`, `engine-ports/src/worktrees.rs:146-147`; `RecordTaskWorktree` reopens the record,
     `rules.rs:5690-5698`).
- **Minimal sequence**: (1) T works in P with uncommitted files; the user cancels it. (2) Card -> the user picks "Delete".
  (3) `apply_answers` deletes P. (4) Later the user reopens T; the node works in P (recreated) and leaves changes.
  (5) Next closer pass (within 30 s): `apply_answers` deletes P again, with whatever is inside.
  Retry/duplicate: the effect is reapplied on every pass forever; with P absent it only costs git calls.
- **Impact**: loss of the uncommitted work of a live task and of its folder under the node; possible deletion of its local
  branch (if it was on the remote).
- **Deterministic test and ablation**: engine with `Worktrees`/`Disk` doubles (`engine-sdk` closer tests or
  `cancelled_worktrees`): cancelled task with dirty P, answered card `DISCARD`, `apply_answers` -> P deleted; `ReopenTask`,
  the double recreates P with an uncommitted file; `apply_answers` again. Today P disappears. Expected: P stays.
  Ablation: remove the guard.
- **Expected fix and files**: apply the answer once and record it (for example `ReleaseTaskWorktree Removed` of the
  `KeptByUser` record, or mark the card as applied), and only while the task is still cancelled and the record of that
  path is still `KeptByUser`; `in_use` must also count the task itself if it is not finished.
  Files: `crates/engine-worktrees/src/cancelled_worktrees.rs`, `task_worktrees.rs`.

#### F-GH-3 · The startup sweep of cancelled tasks deletes a worktree that task handed to another, if the other is already closing or the user kept it
**Class**: CONFIRMED · **Severity**: S2 · **Probability**: low · **Overlap**: none.
- **Invalid state/transition and broken invariant**: a worktree with record `HandedOver` in T1 (cancelled) and open in T2
  (closing), or `KeptByUser` in T2, deleted by T1's sweep. It breaks "HandedOver: ends with that other task"
  (`model.rs:343-344`) and the user's "keep" decision.
- **Evidence**:
  1. `task_worktrees` (task -> launch folder) is never deleted (no `DELETE FROM task_worktrees` exists).
  2. `sweep` chains `stored.get(&task.id)` **without filtering by the end of the record**; it only filters those in
     `task.worktrees` (`cancelled_worktrees.rs:153-159`).
  3. `in_use` ignores tasks that are ending: `worktree_held_by_other` and the plans filter `!t.over()`, and `over()`
     includes `ending` (`rules.rs:508-511`, `lifecycle.rs:1183-1185`).
  4. `settle_path` counts only uncommitted files (`cancelled_worktrees.rs:36-40`), not unpushed commits or ignored files,
     and deletes with `discard_worktree` (`:41-50`).
- **Minimal sequence**: (1) T1 is cancelled; its folder P passes to T2, launched in P (`heir`, record `HandedOver` in T1).
  (2) T2 finishes and its close stops because P has unpushed commits (T2 stays `ending`), or the user picks "Leave" on
  T2's close card. (3) The daemon restarts (every update does that). (4) T1's `sweep`: P exists, is not "in use", nothing
  was asked, 0 uncommitted files -> P deleted.
- **Impact**: the folder disappears under T2's node while it is asked to push its commits (the branch is kept by
  `delete_saved_branch`); ignored files (local configuration, `.env`) and the keep decision are lost.
- **Deterministic test and ablation**: engine with doubles: T1 cancelled with a `task_worktrees` row = P and record
  `HandedOver`; T2 with P open and `ending`; a double with P clean in files but with one local commit;
  `sweep_cancelled_worktrees`. Today P is deleted. Ablation: filter the `stored` path by the end of the record.
- **Expected fix and files**: treat the `task_worktrees` folder as one more record (if the record exists and ended in
  `HandedOver|KeptByUser|UsersOwn`, do not touch it); `in_use` must count as owner the task that has it open even if it
  is ending. Files: `cancelled_worktrees.rs`, `task_worktrees.rs` (`in_use`).

#### F-GH-4 · Closing a task with a merged PR deletes the branch of a remote-less repo (another repo) with commits that exist only there (PR #503)
**Class**: CONFIRMED · **Severity**: S1 · **Probability**: low · **Overlap**: issue #375 (deliver results of repos without a remote) makes this path more frequent; the evidence is new.
- **Invalid state/transition and broken invariant**: commits with no copy anywhere deleted with `git branch -D`. It breaks
  "the branch is deleted when nothing is left to save" (`worktree.rs:668-670`).
- **Evidence**:
  1. `release_all` and the close pass `pr_merged = merged_pr(task).is_some()`: **any** merged PR of the task, in any repo
     (`task_worktrees.rs:190`, `task_closer.rs:179` and `:593`; `rules.rs:1950-1951`).
  2. `unsaved` counts 0 unpushed commits if the repo has no remote (`LocalHome::NoRemote`, `worktree.rs:108-111`,
     `:120-125`): the clean worktree is deleted.
  3. `delete_finished_branch` with `pr_merged=true` skips the "no commits of its own" check and runs
     `git branch -D worktree-<folder>` (`worktree.rs:686-693`).
- **Minimal sequence**: (1) A task works in repo A with a remote (PR merged) and has a worktree in repo B without a remote,
  with commits in `worktree-<t>` not integrated in the local default branch. (2) The task closes. (3) The worktree in B is
  clean -> deleted -> `git branch -D` -> B's commits are left without a reference.
- **Impact**: loss of committed work in the local repo (recoverable only by hand with `git fsck` before a gc).
- **Deterministic test and ablation**: without real git, at the rule level: extract in `crates/system` the "delete branch"
  decision into a pure function over the facts (`remote exists`, `branch contained in the default branch`, `merged PR of
  THIS branch`) and test it; plus a case in the `Worktrees` contract (`engine-doubles/src/contract.rs`) with the double
  modelling a "repo without a remote". Today the double deletes the branch on `pr_merged` alone
  (`engine-doubles/src/worktrees.rs:443-452`), like the adapter.
- **Expected fix and files**: `pr_merged` must mean "the PR whose head is this branch is merged" (same repo and
  `head_ref`), and a branch with commits that are neither on a remote nor in the default branch is never deleted. Files:
  `crates/system/src/worktree.rs`, callers in `task_worktrees.rs` and `task_closer.rs`.

#### F-GH-5 · Post-merge work declared and the node idle: the task stays "Working" forever, its host is never released and it holds a slot
**Class**: CONFIRMED · **Severity**: S2 · **Probability**: medium-low · **Overlap**: the stalled-node recovery work (phases 2-3: contradictions without a timer, `WakeUntaken`) and the node anomaly detection work (signal "turn that ends with no delivery or own work"). Neither names the post-merge case; the evidence is new for them.
- **Invalid state/transition and broken invariant**: a `PostMerge` obligation with nothing that will end it. It breaks "only a
  finished or cancelled state owes nothing; every step has an owner that gives it" (`docs/architecture/task-states.md`,
  "Who owes the next step") and the product rule that a task only waits for capacity or for the user.
- **Evidence**:
  1. Only a fact ends post-merge work (domain-rules `lifecycle.rs:2449-2472`; `task_closer.rs:4-6`).
  2. The node's turn ends idle and there is no deadline for this in `obligation_policy` (only HandInput, Wake, Answer).
  3. `can_release_host` is false while post-merge is owed (`domain/src/lifecycle.rs:1407`): `idle_release` does not close
     the host, so the "host end -> resumed session or card" (`observations.rs:681-701`) never arrives either.
  4. The closer only restarts its row (`hold_while_working_after_merge`, `task_closer.rs:637-645`), without a message.
  5. The obligation shown belongs to Alondra (`Finishing -> (Alondra, Close)`, `obligation.rs:59`) or Working(AfterMerge)
     (`domain/src/lifecycle.rs:1522-1523`); `holds_slot` counts `PostMerge` (`:1429-1440`).
- **Minimal sequence**: (1) The node declares `post_merge_work{reason}` and delivers. (2) The user accepts, Alondra merges,
  `post_merge_reminder` reaches the node (once, `rules.rs:2559-2566`). (3) The node launches something, ends its turn
  without `post_merge_work` without a reason, and leaves no background work (or leaves it and when it ends the Wake opens
  a turn that again ends without closing). (4) Nothing else happens: task "Working", host alive, slot held.
- **Impact**: a stuck task that the user can only free if they notice and write to the node; host memory and a gate slot
  lost indefinitely.
- **Deterministic test and ablation**: pure domain or `crates/machine-model`: declare post-merge, merge, end the turn with
  the session idle and no work; run the virtual clock: today the model detects nothing (the session "is alive"). Check
  `host_releasable == false` and an obligation with no pending fact. Ablation: n/a (it is the absence of a rule).
- **Expected fix and files**: without a timer (product decision), treat "turn ended idle with `PostMerge` owed, no own work
  and no pending input" as a lifecycle contradiction: give the turn back to the node with the reminder (like `Wake`) and,
  if it happens again, raise a `PostMergeProblem` card to the user. Files: `crates/domain` (end-of-turn observations),
  `domain-rules/src/lifecycle.rs`.

#### F-GH-6 · An attachment of a user message to an open task is deleted after 30 days even though the task is still open
**Class**: CONFIRMED · **Severity**: S3 · **Probability**: low · **Overlap**: STORE (M-STORE-13) does not cover the files folder; new.
- **Invalid state/transition and broken invariant**: `files/uploads/<id>` deleted while a live entry of the task names it.
- **Evidence**:
  1. The message carries the absolute path of the attachment in its text (`daemon-engine/src/domain_messages.rs:65-76`,
     `uploads::delivery_note`) and is stored as a lifecycle entry, not as a card.
  2. `References::of` scans only `interactions` and `document_revisions` (`files_retention.rs:205-220`).
  3. No reference -> "without owner" -> 30 days from its mtime (`:163-176`, `:178-180`).
  4. Since #503 the hourly pass deletes it (`engine-kernel/src/retention.rs:21-24`).
- **Minimal sequence**: (1) The user sends a task a message with an image. (2) The task stays open (paused, waiting on
  dependencies, scheduled) for more than 30 days, or the message waits for a paused node. (3) The hourly pass deletes the
  image; the node, on reading it or after a session handover, does not find it.
- **Deterministic test and ablation**: `files_retention` tests, no git: a state with an open task whose journal has a `Told`
  with `files/uploads/u-1/x.png` and the file with an mtime 31 days old; `sweep` today marks it `remove`. Ablation: also
  index entries of open tasks and request bodies.
- **Expected fix and files**: `References::of` must include texts of entries of non-finished tasks, bodies of open
  requests and scheduled launches. File: `crates/daemon-support/src/files_retention.rs`.

#### F-GH-7 · A merge that GitHub blocks for a required review goes back to the node and invalidates the user's acceptance, in a loop
**Class**: CONFIRMED · **Severity**: S2 · **Probability**: medium in repos with a mandatory review, none without one · **Overlap**: none.
- **Invalid state/transition and broken invariant**: the node receives "fix and deliver again" for something only a person can
  give on GitHub, and the user's acceptance is invalidated. It breaks the distinction the merger itself makes for checks
  that ask for action ("card and wait", `domain_merges.rs:353-356`).
- **Evidence**:
  1. `gh pr merge` fails ("A review is required").
  2. `merge_failed` (`domain_merges.rs:461-500`): state Open, same head, not queued; `merge_state == Other` (BLOCKED) with
     all required checks passed -> continues; `can_merge` = true (write permission) -> not `timed_out` and not `Clean` ->
     `failed` -> `hand_back` with `Instruction::Conflicts` (`:563-573`, `:671-681`) -> `invalidate_feedback`
     (`rules.rs:4960-4964`, `:1709-1726`).
  3. The test `blocked_without_required_checks_is_left_to_githubs_answer` (`domain_merges/tests.rs:333-340`) only checks
     that the merge is attempted.
- **Minimal sequence**: a repo with a mandatory review; the user accepts; Alondra tries the merge; GitHub rejects it; the
  node receives the rejection and delivers the same head again; the user accepts again; this repeats until someone
  approves on GitHub.
- **Impact**: duplicated work by the node and the user; a new delivery card on every round.
- **Deterministic test and ablation**: engine with `FakeGithub`: `open_blocked(7, HEAD, Checks::None)` and `merges[7] =
  Err("...review is required")`; accept; `merge_due`. Today: feedback to the node and invalid acceptance. Expected: a
  "needs a review on GitHub" card and waiting, acceptance intact. Ablation: remove the new branch in `merge_failed`.
- **Expected fix and files**: read `reviewDecision` (or the reason for BLOCKED) in `Status`; with a pending review, wait and
  tell the user as with `ActionRequired`, without `hand_back`. Files: `crates/github-gh/src/merges.rs`,
  `crates/engine-ports/src/github.rs` (`Status`), `crates/engine-github/src/domain_merges.rs`.

#### F-GH-8 · The GitHub API limit still raises technical cards: one per PR in the watcher and another per PR in the merger
**Class**: CONFIRMED (behavior; what exactly the earlier change that stopped GitHub rate-limit cards intended is not in this history) · **Severity**: S2 · **Probability**: low (only with the limit exhausted for more than 5 min) · **Overlap**: the earlier change that stopped GitHub rate-limit cards (done) did not reach these two paths; the evidence is new.
- **Invalid state/transition and broken invariant**: a card to the user for a wait that resolves itself (product rules "no
  technical cards" and "cards only when actionable"). The only code that recognizes the limit is `HttpAnswer::rate_limited`
  in the read of required checks (`github-gh/src/merges.rs:184-190`) and in the reports (`cli-report/src/report/github.rs:157`):
  that is the visible effect of the finished task.
- **Evidence**:
  1. The observer returns `gh`'s stderr as an unclassified `String` (`github-gh/src/cli.rs:54-58`).
  2. `ObservationFailures::failed` raises «Alondra no puede leer el PR» ("Alondra cannot read the PR") after 5 consecutive
     failures, per PR (`domain_github.rs:268-296`).
  3. In the merger, `GhFailure` has no limit case (`engine-ports/src/github.rs:83-107`) and `raise_trouble` raises "Alondra
     can't merge PR... check gh auth status" after 15 min of failures (`domain_merges.rs:623-643`).
- **Minimal sequence**: with 20 watched PRs and the GraphQL budget exhausted, every `gh pr view` fails for the hour; after
  5 minutes there are 20 cards «no puedo leer el PR #n» ("I cannot read PR #n"); an accepted PR adds another at 15 min.
- **Deterministic test and ablation**: domain with `FakeObserver`/`FakeGithub`: "API rate limit exceeded" failures 5 times
  -> today there is a card. Expected: none; the task shows "waiting for the GitHub API" (`Room(capacity)`). Ablation:
  remove the classification.
- **Expected fix and files**: classify the limit in `github-gh` (`gh` text / HTTP 429/403 with `x-ratelimit-remaining: 0`)
  as a transient failure with a reset time; do not count it toward cards; space the reads until the reset.

#### F-GH-9 · The merger rereads GitHub on every domain commit for a PR closed outside or with a new head, until the watcher sees it
**Class**: CONFIRMED · **Severity**: S3 · **Probability**: medium · **Overlap**: none stated; it brings the call volume close to the F-GH-8 limit.
- **Evidence**:
  1. `step` returns `Settled` without recording anything when GitHub says Closed or Open with another head
     (`domain_merges.rs:303-307`).
  2. `Settled` clears `waited` (`:188-190`), so the PR is still "due" (the domain has it Open with the old head, `due`,
     `:52`).
  3. The loop, which wakes on every commit (`:695-701`), calls `gh pr view` again without waiting for `poll`. This lasts
     until the watcher's next round (at most 60 s), with dozens of commits per second when nodes are active.
- **Impact**: bursts of calls to GitHub that approach the limit of F-GH-8.
- **Deterministic test and ablation**: `FakeGithub.reads`: accepted PR; GitHub says it is Closed; 10 unrelated domain commits
  -> today `reads` grows by 10. Expected: 1. Ablation: record the observation or keep `waited`.
- **Expected fix and files**: record what was read (`observe_status`) in those two cases, as it already does for Merged.

### Hypotheses

#### H-GH-1 · An old observation applied late invalidates an acceptance
`ObservePrState` carries no version; any different head invalidates (`rules.rs:5128-5130`). `apply` compares with the round's
snapshot (`domain_github.rs:315-318`), so it needs the snapshot to be older than the read and the read older than the domain
(two pushes within one watcher round, or the merger/`catch_up_task` writing in between). Missing: measure whether it happens
with auto-merge. Overlap: the stable pull request identity work ("repeated or out-of-order observations").

#### H-GH-2 · Moving an already-merged PR resurrects it as Open
`RegisterPr` from another task on a PR of a terminal task writes `state: Open, merged_head: None` without looking at the state
(`rules.rs:4668-4672`); the next observation turns it back into Merged as "just merged" (`rules.rs:5127`, a notice and a
possible `merged_delivery` of the new task). Missing: check whether any real flow registers a merged PR. Overlap:
the stable pull request identity work.

#### H-GH-3 · PR images deleted while the cache thinks they are fetched
`pr_images::keep` stores into `files/images` (`pr_images.rs:198-206`); no card names them, so after 30 days from their mtime
they are "without owner"; the in-memory cache (`pr_details.rs:258-264`) keeps returning them until restart. Missing: see
whether `markdown::keep` rewrites the file (and renews the mtime) when it already exists.

#### H-GH-4 · Restart with a PR in the merge queue
`queued` is lost; if the user authorized and checks are missing that the account can skip, `step` would call
`gh pr merge --admin` on a queued PR (`domain_merges.rs:369-381`, `:433-437`). Missing: what GitHub does with `--admin` on a PR
in the queue.

#### H-GH-5 · The scratch of a reopened task no longer exists
`finish` deletes `files/tasks/<id>` on close (`task_closer.rs:290-293`); a new session that took over the conversation reads
`conversation-<session>.md` from there (`session_refresh.rs:197-208`). After a reopen, the resumed session may ask for a deleted
file. S3. Missing: confirm that a reopened task's new session still looks for that file.

## Orchestrator and schedules (`ORCH`)

Scope: `crates/engine-orchestrator` (executor, recovery, watchdog, inbox, providers),
`crates/daemon-engine/src/domain_orchestrator_tools.rs`, `crates/core/src/orchestrator.rs`, `orchestrator.json`,
the `cancel|acknowledge|retry` subcommands of the orchestrator CLI, the `Answer` deadline
(`daemon-engine/src/obligation_watch.rs` + `orchestrator_inbox::overdue`), and schedules
(`domain-rules/src/schedule.rs`, `engine-sdk/src/domain_schedule_runner.rs`, `domain_schedule_api.rs`, tables
`schedules`, `schedule_runs`, `schedule_failed_launches`, `auto_launches`, `schedule_paid_accounts`). Audited at commit
`1567967b9`, by reading only.

### M-ORCH-1 · Orchestrator execution (`orchestrator.json`, `Runtime`)
- **Kind**: implicit, composite. **Defined in**: `domain_orchestrator.rs:37-71` (`Runtime`), `:82-95` (`Failure`),
  `:97-104` (`Run`).
- **Source of truth**: the file `HOME/orchestrator.json` (written as `.next` + rename, `:125-132`), with no lock between
  writers. **Copies and derivatives**: `DomainRuntime.orchestrator_rests` (atomic, reloaded on every tick `:457`),
  `Orchestrator.runs/stop/host` in memory (`engine.rs:16-26`), rows `orchestrator_batch_runs`/`delivery_attempts`
  (M-ORCH-2/3). `status()` derives the phase (`:964-981`). The file and the database are written in different
  transactions, so they can diverge; `recovery.rs` exists to reconcile them.
- **States** (a combination of fields):

| State | active_run | last_error | failure | retry_at | Notes |
|---|---|---|---|---|---|
| Idle | None | None | None | None | `status` = idle |
| Backoff | None | Some | Launch | Some(future) | `failed_batch` without a problem `:756-762` |
| Running | Some(spawned=false/true) | None | None | None | `cancel_requested` may be true |
| Ambiguous | Some | Some | Interrupted/Denied/Cancelled | None | card `problem-orchestrator-<run>`; `status.can_retry` |
| Limited | -> Idle | None | None | None | with `batch_handoffs` if there were effects `:637-649` |
| Failed with no exit | None | Some | Launch | None/past | see F-ORCH-3; `status` = failed, no actions |

- **Events**: tick (claim -> execute -> `BatchEnd`), `handled`, `failed`, `cancel/acknowledge/retry` (HTTP and the answer to
  the card through `apply_problem_answers` `:831-871`), `recover`/`startup_recovery`/`repair_incomplete`, watchdog.
- **Allowed transitions** (essential):
  - Idle -> claim [selection ok, no pause, pending messages or resolutions] -> Running (`:507-530`).
  - Running -> `Handled` -> Idle (`handled` `:673-711`: `finish_batch`, `settle_orchestrator_batch(Handed)`,
    `announce_request_tasks`, `rested`, `hand_replans`, `settle`).
  - Running -> `NotStarted`/no effects -> Backoff (`:756-762`, 30 s x 2^n, max 16 min).
  - Running -> `Interrupted`/`Denied`/`Cancelled{effects}` with doubtful effects -> Ambiguous + card (`:764-773`).
  - Running -> `Interrupted` with confirmed effects -> handoff + Backoff (`:658-665`).
  - Running -> `Limited` -> Idle (+handoff).
  - Ambiguous -> acknowledge -> Idle with `pending_resolutions` (`:313-337`); -> retry -> Idle, messages `retryable`
    (`:340-362`).
  - Daemon start: `startup_recovery` -> `repair_incomplete` (`recovery.rs:4-345`); see F-ORCH-1.
- **Rejected**: `acknowledge/retry` of another run -> `bail!` -> HTTP 409 (`:1001-1017`); `decide` requires `last_error`
  except for `Cancel`.
- **Terminals**: none; it is a cycle.
- **Invariants**: assumed, not enforced: `failure.is_some() => last_error.is_some()`; Ambiguous => an open card.
  Representable but meaningless: `active_run=None AND failure=Denied`.
- **Persistence and restart**: the file survives; `runs`, `stop`, `host`, the watchdog and the deadline clock are lost.
  An empty `runs` after a restart is exactly what makes a batch "cold" (F-ORCH-1).
- **Concurrency**: one loop (`run` `:1020-1066`) with `Ownership` (a file lock between processes) and `HostGuard` (one
  host). But the HTTP server (`decide`) and the closure `mark_spawned` (`:542-552`) do load -> modify -> save of the same
  file without a lock (F-ORCH-7).

### M-ORCH-2 · Delivery of each message to the orchestrator (outbox)
- **Kind**: explicit in SQL (chains with a version CAS). **Defined in**: `store-sqlite/src/lib.rs:1697-1798`, `:1806-1891`;
  `batch_recovery.rs:65-110`.
- **Source of truth**: `domain_outbox.state` + `delivery_attempts(state,version,reason)` +
  `orchestrator_deliveries(attempt,session)`.
- **States**: pending -> dispatching (claim) -> handed | retryable | uncertain | discarded; uncertain -> retryable |
  discarded (`resolve_uncertain`, reason `explicit_user_reconciliation`). `discard_expired_orchestrator_messages` moves
  pending/retryable to discarded after 24 h (`lib.rs:1625-1640`, TTL in `engine-ports/src/journal.rs:47`).
- **Guards**: `finish_message` only from `dispatching` with the expected version (`lib.rs:1767`);
  `settle_orchestrator_batch` does not overwrite a newer attempt of the same message (`batch_recovery.rs:98`).
- **Terminals**: handed and discarded; retryable goes back to pending for the purposes of `pending_messages`.

### M-ORCH-3 · Batch (run) in the database
- **Kind**: implicit, composite. **Defined in**: `store-sqlite/src/batch_effects.rs:6-12` (schema).
- **Source of truth**: `orchestrator_batch_runs(run, session, data_epoch, selection, ended)`,
  `orchestrator_batch_decisions(target)`, `orchestrator_tool_operations(state started|succeeded|uncertain)`,
  `orchestrator_batch_effects(pinned)`, `orchestrator_batch_reports`.
- **States**: open (`ended=0`) -> with a user decision -> finished (`finish_batch` `:203-212`). Operations: started ->
  succeeded | uncertain.
- **Cross-cutting use**: `user_notices_in_open_passes` (`lib.rs:1576-1581`) holds the user notices of cards created in a
  batch with `ended=0` (rule 10).
- **Persistence and restart**: everything survives; `unfinished_orchestrator_batches` (`batch_recovery.rs:7-47`) is the
  entry point of recovery.

### M-ORCH-4 · In-memory state of the executor
- **Kind**: implicit. `Orchestrator { host, stop, runs }` (`engine.rs:16-26`), `Watchdog { progress, since, fired,
  restart_pending }` (`watchdog.rs:49-58`), `DomainRuntime.orchestrator_progress`
  (`engine-kernel/src/domain_runtime.rs:126-132`), `orchestrator_rests`. All of it is lost on restart. The watchdog fires
  after 5 min without progress while requests without a task exist (`watchdog.rs:80-99`, `:148-185`) and restarts the
  executor (`:191-234`).

### M-ORCH-5 · Settling the inbox (`orchestrator_inbox::steps/settle`)
- **Kind**: implicit (numbered letters keyed by id). **Defined in**: `orchestrator_inbox.rs:52-72`, `:207-252`, `:265-275`.
- **Events and transitions, per request without a task**: message -> `Close`; request -> `UnsplitNotice-1` (to the
  orchestrator) -> `UnsplitProblem-n` (to the user) -> answer `close` -> `Close(withdrawn)`; `retry` -> a new round.
  Suggestion -> `UnsettledNotice` once.
- **Guard**: only when nothing is pending for the orchestrator and (in `tick`) `last_error` is None
  (`domain_orchestrator.rs:488-493`).
- **Persistence and restart**: idempotent: the letter id is derived; `carry_out` refuses to create an existing letter
  (`:443-447`).

### M-ORCH-6 · `Answer` deadline (what the orchestrator owes a task)
- **Kind**: implicit. The clock is `obligation_watch.rs:46-55,119-152` (in memory); the actions are
  `orchestrator_inbox::overdue` (`:76-96`), `stalled` (`:263-283`), `unanswered` (`:303-317`), `unreviewed` (`:321-329`).
- **Sequence per case**: overdue the first time -> a `Tell` reminder (a letter to the orchestrator) and the clock restarts
  (`enforce` `:157-169`); overdue the second time -> `SettleStall(Instruct|Unsettled)` / `Withdraw` of the question /
  `SettlePlanReview(Unchanged)`.
- **Freezing**: only with the usage pause (`orchestrator_held` `:122-126`). An orchestrator blocked on an ambiguous-batch
  card does not freeze the clock (by design: "busy or failing").
- **Persistence and restart**: the clock goes back to zero; reminders already emitted (letters) survive.
- **Overlap**: phase 4 of the stalled-node recovery work replaces this deadline with `AnswerSkipped`.

### M-ORCH-7 · Replanning (`Owed::Replan`)
- **Kind**: implicit. **Defined in**: `orchestrator_inbox.rs:97-118`, `:286-301`, `:334-360`; the obligation in
  `domain-rules/src/ownership.rs:337-355`.
- A task that waits for a cancelled one, or whose launch plan was refused: at every "rest" (`rests`) of the orchestrator a
  letter `dependency-cancelled-<t>--<d>-<rest>` / `plan-refused-<card>-<rest>` is created. No deadline, no cap. See F-ORCH-2.

### M-ORCH-8 · Retention of announcements (rule 10)
- **Kind**: implicit. `announce_request_tasks` runs only from `handled` (`domain_orchestrator.rs:681`); notices of the
  batch's cards are held while `orchestrator_batch_runs.ended=0` (`lib.rs:1576`, filter in
  `daemon-server/src/domain_delivery.rs:43-44`).

### M-ORCH-9 · Schedule (`Schedule`)
- **Kind**: explicit (a validated struct). **Defined in**: `domain-rules/src/schedule.rs:25-45`, `:186-259`.
- **Source of truth**: table `schedules` (+ `schedule_paid_accounts` for the paid-account grant,
  `store-sqlite/src/provider_accounts.rs:15-24`).
- **States**: enabled with `next_run`; enabled without `next_run` (a consumed Once); disabled; deleted (tombstone).
- **Invariants**: enforced: `!enabled => next_run=None`; deleted => disabled; valid interval and anchor.

### M-ORCH-10 · Scheduled run (`ScheduleRun` x `ScheduleLaunch`)
- **Kind**: explicit, composite. **Defined in**: `schedule.rs:88-176`, transitions `:396-660`, commands in
  `domain-rules/src/rules.rs:3145-3311`, runner `engine-sdk/src/domain_schedule_runner.rs:303-690`.
- **States**: run: Launching -> Sent | Failed | Interrupted. Launch: Pending -> Dispatching -> Handed | Failed;
  Dispatching -> Uncertain (when the store is opened, `store-sqlite/src/lib.rs:734-735`) -> Handed | Failed.
- **Composite with**: the task (agent_id/started), the card `problem-launch-<run>` (`rules.rs:3264-3305`), the retry run
  `retry-<run>` (`domain_schedule_runner.rs:557-597`), `schedule_failed_launches` (`store-sqlite/src/repository.rs:257-275`)
  and the admission queue (`Key::Run`).
- **Key guards**: only one `Launching` run per schedule (`schedule.rs:413-423`); `claim_launch` only from Pending
  (`:538-551`); `scheduled_launch_guard` requires a task without a node (`rules/scheduled_launch.rs:52-74`).
- **Terminals**: Failed and Interrupted of the run; Handed/Failed of the launch. Interrupted consumes the slot without a
  card (by design).
- **Concurrency**: one loop (`run` `:788-821`); any error leaves with `?` and the daemon supervisor ends the process
  (`alondrad/src/lib.rs:245-251`). See F-ORCH-5.

### M-ORCH-11 · Automatic launch (`auto_launches`)
- **Kind**: implicit. `launching` -> `launched` | `blocked(reason)`; deleted to retry (`engine-sdk/src/auto_launch.rs:62-77,102-176`,
  `store-sqlite/src/lib.rs:255,1327-1431`). It belongs mostly to admission; here only its race with `create_task` counts
  (F-ORCH-8).

### M-ORCH-12 · Legacy reducer `core::orchestrator`
- **Kind**: explicit. `Phase { Idle, Starting, Running, Compacting, Failed }` with `validate/apply`
  (`core/src/orchestrator.rs:13,80-200`). The current executor does not use it (only `NODE_ID`); it remains as an old view
  event (`core/src/event.rs:98`). It is a duplicate machine that no longer describes the real one: any read of
  `view.orchestrator` contradicts `orchestrator.json`. No reachable finding of its own.

### Findings

#### F-ORCH-1 · A daemon restart in the middle of a batch stops the orchestrator until the user answers a card
**Class**: CONFIRMED · **Severity**: S2 · **Probability**: medium: batches are frequent (every request, reminder or case) and the daemon restarts on every version · **Overlap**: the provider adapters work (phase 6) moves the orchestrator onto the adapters (it may touch this recovery): the evidence is NEW. Not in open issues. `git log` of the file shows no fix after d3dcbf2c9, which introduced `cold_batch`.
- **Invalid state/transition and broken invariant**: the orchestrator stays Ambiguous (`active_run` + `last_error`,
  `failure=Interrupted`) with a `problem-orchestrator-<run>` card for the user, caused by a technical fact of Alondra's
  own (its restart), and processes no request until that answer. It breaks the product rule that stalls are fixed by
  Alondra and never become a user card, and the claim of the test at `domain_orchestrator.rs:1970` ("A daemon restart
  during an execution resolves it without the user"), which only exercises `recover()`, a path the real startup does not
  take for current batches.
- **Evidence**:
  1. Every batch is claimed with a selection: `claim_selected_orchestrator_batch(..., Some(&selection))`
     `domain_orchestrator.rs:509-510` (writes `orchestrator_batch_runs.selection`, `store-sqlite/src/lib.rs:1722-1728`);
     the in-memory `runs` receives the run `:511`.
  2. Neither SIGTERM nor the handover closes the batch: `alondrad/src/lib.rs:330-347` exits without touching the executor.
  3. Startup: `run` -> `startup_recovery` (`domain_orchestrator.rs:1035-1041`) -> `repair_incomplete` (`recovery.rs:5`). The
     active run appears in `unfinished_orchestrator_batches` (`batch_recovery.rs:13`, `r.run=?2`).
  4. With no decision, `dispatching` attempts -> `unresolved` (`recovery.rs:96-99`); not `handed`, not `ended`;
     `last_error` None.
  5. `confirmed_batch_effects` with `admitted=[]` (`recovery.rs:192-205`): if there was a `started` operation, it already
     fails here with a card; if not, it goes on.
  6. `cold_batch` (`recovery.rs:226-243`): `!automatic` (dispatching attempts), `selection.is_some()`, `!batch.ended`, no
     handoff, `runs` empty after the restart, and `batch_completed` false unless the final report already arrived
     (`batch_effects.rs:44-47`).
  7. `recorded_stop = Interrupted("The daemon restarted without a durable terminal report...")` (`recovery.rs:244-245`) ->
     `failed(..., Some(problem))` (`recovery.rs:254-268`) -> `failed_batch` stores Ambiguous and raises the card
     (`domain_orchestrator.rs:764-773`).
  8. From then on `tick` leaves at `:464` (`active_run.is_some()`); the watchdog is suspended (`watchdog.rs:154-163`).
- **Minimal sequence**:
  1. A new request arrives.
  2. The orchestrator claims the batch and its model works.
  3. An update (or any restart) happens before the result.
  4. The daemon starts: an "interrupted batch" card goes to the user.
  5. New requests wait unprocessed until the user picks "continue" or "retry".
- **Impact**: all new orchestrator work (requests, replans) stops because of a routine restart; the user gets a technical
  card. `Answer` deadlines do keep running (M-ORCH-6).
- **Deterministic test and ablation**: engine-orchestrator, no host: seed like
  `tests/recovery_decisions.rs::batch_with_confirmed_effect(f, true)` but without a confirmed effect:
  `claim_selected_orchestrator_batch` with a selection and a stored `Runtime{active_run}`; open a new `DomainRuntime` over
  the same HOME (like `restarted` at `:135-139`) and call `startup_recovery()`. Expected: no `problem-orchestrator-*`
  interaction, `active_run` None and the batch offered again or passed on as a handoff. Today: card and Ambiguous.
  Ablation: removing the `or_else(cold_batch...)` at `recovery.rs:244` makes it pass.
- **Expected fix and files**: a cold batch with no uncertain operations resolves without the user: confirmed effects ->
  handoff; no effects -> `reconcile_orchestrator_batch(Retryable)`. Only a `started|uncertain` operation justifies asking.
  Files: `domain_orchestrator/recovery.rs`, the test at `domain_orchestrator.rs:1970` (it should use `startup_recovery`).

#### F-ORCH-2 · Replanning reminders feed themselves: one batch per reminder, without end
**Class**: CONFIRMED · **Severity**: S2 (duplicated work) · **Probability**: low-medium: it depends on the model not complying or its action being rejected (for example a plan that `update_launch_plan` keeps rejecting) · **Overlap**: the note in `docs/architecture/task-states.md` ("Waiting for a cancelled task advances only when the orchestrator is idle and nothing fails; a task will give it an owner and a deadline") addresses the opposite problem; this loop is NEW evidence.
- **Invalid state/transition and broken invariant**: a life loop without progress. Every batch that ends "rests"
  (`rests+1`), and every rest creates a new reminder, which is a message to the orchestrator, which triggers another batch.
  If the orchestrator does not decide (or its decision is rejected), model batches are launched without limit.
- **Evidence**:
  1. `handled` -> `rested` (`domain_orchestrator.rs:688`, `:740-743`) -> `hand_replans` (`:699`); also `failed`
     (`:746-748`).
  2. `replans(state, rest)` (`orchestrator_inbox.rs:102-110`): an id with the rest number, `inbox_card(..., rest as u32)`
     (`:343` cancelled dependency, `:288` refused plan); the new id does not exist -> `Step::Tell`.
  3. `carry_out` -> `PublishInformation` from System to Orchestrator (`engine-kernel/src/domain_runtime/problems.rs:205-213`)
     -> effect `NotifyOrchestrator` (`domain-rules/src/rules.rs:3986-3991`) -> pending message.
  4. `handled` sees the pending message and leaves (`domain_orchestrator.rs:700-702`); the `run` loop wakes on the change
     (`:1059-1063`) and `tick` claims a new batch (`:483-530`).
  5. Nothing limits it: the test at `orchestrator_inbox.rs:656-689` fixes "one more at each rest" and does not simulate the
     reminder being the cause of the rest.
- **Minimal sequence**:
  1. A and B exist; B depends on A.
  2. The user cancels A.
  3. Reminder 1 -> batch 1; the orchestrator does not call `set_dependencies`/`cancel_task` (or the call is rejected).
  4. Batch 1 ends -> reminder 2 -> batch 2 -> ...
- **Impact**: unbounded token spend and a busy orchestrator; the task still does not get decided.
- **Deterministic test and ablation**: orchestrator_inbox, pure domain with `wiring::open`: `pair_of_tasks`; repeat N times a
  "batch that does nothing": take the orchestrator's pending messages, mark them `handed`, `rest(&domain)`. Assert that the
  number of new messages for the orchestrator stays bounded (for example at most 1 per external event). Today it grows
  with N. Ablation: with `rest as u32` changed to a fixed number it stops growing.
- **Expected fix and files**: a rest caused only by replanning reminders does not generate another; or growing spacing; or
  after K reminders with no change, a factual task state (no card). Files: `orchestrator_inbox.rs` (`replans`,
  `hand_replans`), `domain_orchestrator.rs` (`handled`).

#### F-ORCH-3 · After 24 h of launch failures, requests are left untreated and the watchdog restarts in vain every 5 min
**Class**: CONFIRMED · **Severity**: S2 · **Probability**: low (24 consecutive hours of launch failures) · **Overlap**: none known.
- **Invalid state/transition and broken invariant**: "Failed with no exit" (M-ORCH-1): `active_run=None`,
  `last_error=Some`, `failure=Launch`, no pending messages. No transition leaves it except a new message from another
  source; open requests without a task receive no reminder or card, and the orchestrator status command shows `failed`
  with no actions (`can_retry` and `can_cancel` false).
- **Evidence**:
  1. Launch failure: `failed_batch` without a problem leaves `last_error`, `retry_at`, `failure=Launch` and the attempts
     `retryable` (`domain_orchestrator.rs:752-762`).
  2. Past `retry_at`, `tick` discards the messages older than 24 h (`:476-481`, TTL `engine-ports/src/journal.rs:47`, SQL
     `store-sqlite/src/lib.rs:1625-1640`).
  3. With no pending messages or resolutions: `hand_replans` and `settle` run only `if runtime.last_error.is_none()`
     (`:488-494`). `last_error` is cleared only in `handled`, `acknowledge`, `retry`, `recover` and a new claim (`:525`),
     which does not happen.
  4. Watchdog: requests without a task exist and `Launch` does not suspend it (`watchdog.rs:154-163`) -> it fires after
     5 min, `startup_recovery` does nothing, it clears `retry_at` (`:225-229`) and re-arms: this repeats every 5 min.
  5. `status`: `phase=failed`, `can_retry = active_run && last_error` = false, `can_cancel` = false (`:964-981`).
- **Minimal sequence**:
  1. A request R arrives.
  2. The orchestrator does not start for 24 h (no host, provider error) and keeps retrying with `Launch` failures.
  3. On the next tick, R's message expires.
  4. The provider comes back: there is no batch; R stays open without a task until another request or case brings a
     message.
- **Impact**: user requests with no result or card (the inbox is the channel for every request); the log fills with watchdog
  restarts.
- **Deterministic test and ablation**: engine-orchestrator, no host: store `Runtime{last_error:Some, failure:Some(Launch),
  retry_at:Some(past)}`, create a request, put its message in `retryable` and its time more than 24 h back with
  `set_message_created_at_for_test`; `tick`. Expected: `last_error` clear and `request-unsplit-<r>-1` (or a batch).
  Today: nothing changes. Ablation: removing the condition at `:491` makes it pass.
- **Expected fix and files**: with no messages to retry, a launch failure stops blocking settlement (clear `last_error` when
  the last claimed message is discarded, or settle anyway); the watchdog does not restart for a legitimate backoff.

#### F-ORCH-4 · A scheduled launch that did start is considered failed after a restart, and answering the card cancels the live task
**Class**: CONFIRMED (the displayed state is inferred; the chain failure -> card -> cancellation is read end to end) · **Severity**: S2 · **Probability**: low (a window of seconds during host start, coinciding with a restart) · **Overlap**: none.
- **Invalid state/transition and broken invariant**: run `Failed` + `problem-launch-<run>` ("could not start") over a task
  that was admitted with a node and a running host. Both options of the card cancel that task, and "Try again" also
  launches a second execution.
- **Evidence**:
  1. `dispatch_pending`: `ClaimScheduleLaunch` (Dispatching) `domain_schedule_runner.rs:456-471` -> `launch_through_sdk` `:473`
     -> `sdk_owner::launch` -> `start_bound` confirms `AdmitExecution` in the domain (`engine-sdk/src/sdk_owner.rs:539,576`)
     and then starts the host (`:585`).
  2. `RecordScheduleLaunchHanded` comes afterwards (`domain_schedule_runner.rs:477-482`). If the daemon dies between 1 and 2,
     when the store opens Dispatching -> Uncertain (`store-sqlite/src/lib.rs:735`); the task already has a node and the
     daemon adopts or launches it (`sdk_owner.rs:774-806`, "a daemon that ended right after admitting it").
  3. `run_cycle` -> `recover_uncertain` (`domain_schedule_runner.rs:493-515`) -> `fail_run` without checking whether the task
     has a node.
  4. `RecordScheduleRunFailed` accepts Uncertain (`schedule.rs:602-605`) and, with the task open, inserts the card and
     `NotifyUser` (`rules.rs:3270-3301`).
  5. `answer_failed_launches`: "retry" -> `retry_launch` creates `retry-<run>`; "seen" -> `(true,false)`; in both cases
     `CancelTask` of the original task (`domain_schedule_runner.rs:526-546`).
  6. The tests cover only the task without a node (`:1259-1301`, `:1345-1389`: "nothing is assigned the uncertain run's
     task").
- **Minimal sequence**:
  1. The time of a schedule arrives.
  2. The launch is claimed and the execution admitted.
  3. The daemon restarts (an update) while the host starts.
  4. The host is adopted and the node works.
  5. "Scheduled task ... could not start" appears.
  6. The user picks any option: the live task is cancelled (and with "Try again" another one is launched).
- **Impact**: loss of work in progress and duplicated work; a false visible state.
- **Deterministic test and ablation**: engine-sdk, `domain_schedule_runner` with its `Fixture`, no host: `claim` +
  `finish_claim` + `ClaimScheduleLaunch`; admit the task to an agent with domain commands (`RegisterAgent` + the admission
  `start_bound` uses); reopen with `Fixture::open` and `run_cycle`. Expected: launch Handed with that agent, no
  `problem-launch-*`. Today: run Failed and a card. Ablation: with the fix in `recover_uncertain` reverted the test fails.
- **Expected fix and files**: `recover_uncertain` checks the task: with `agent_id`, it records `RecordScheduleLaunchHanded`
  with that agent; only without a node does the run fail. `answer_failed_launches` never cancels a task with a node. File:
  `domain_schedule_runner.rs`.

#### F-ORCH-5 · A non-domain error in the schedule runner stops the whole daemon; with an unreadable `options.toml` it enters a restart loop
**Class**: CONFIRMED for the propagation; the concrete trigger requires "non-modern" options (no connected providers and no model controls), so that the execution has no `selection` · **Severity**: S1 (everything stopped) · **Probability**: low · **Overlap**: none.
- **Invalid state/transition and broken invariant**: the daemon process ends because of a recoverable failure of a scheduled
  execution; on return, the same Pending execution brings it down again.
- **Evidence**:
  1. `run` uses `run_cycle(...).await?` (`domain_schedule_runner.rs:798,810`); the supervisor does `bail!` with "Schedule
     worker failed" and the daemon exits (`alondrad/src/lib.rs:245-251`).
  2. In `dispatch_pending`, without `selection` the account is chosen with `choose_for_launch`, which tolerates broken
     options (`daemon-engine/src/accounts.rs:178-179`, `unwrap_or_default`); with a slot in the queue, `options_at(home)?`
     (`:415`) fails if `options.toml` cannot be parsed or `options.pending` exists
     (`daemon-engine-model/src/options.rs:49-62`).
  3. The execution is still Pending (not yet claimed) and is retried at every start.
  4. Other equally hard exits: non-ordinary errors of `ClaimScheduleLaunch` (`:466-470`), of `RecordScheduleLaunchHanded`
     (`:483-486`) and of the slot claim (`:667`). A storage error in `RecordScheduleLaunchHanded` brings the daemon down
     right after launching, which is the entry of F-ORCH-4.
- **Minimal sequence**:
  1. A scheduled execution is left Pending waiting for a slot.
  2. `options.toml` becomes unreadable (a manual edit).
  3. A slot frees up: the daemon exits.
  4. launchd starts it; step 3 repeats.
- **Impact**: the whole daemon goes down and keeps going down until someone fixes the file, with no visible hint in the app.
- **Deterministic test and ablation**: engine-sdk, doubles of the queue and of accounts: a Pending execution with options
  without `connected`, write an invalid `options.toml` and call `run_cycle`. Expected: `Ok` and the execution Pending with
  its reason (or `fail_run`). Today: `Err`. Ablation: with the `?` at `:415` reverted, it fails.
- **Expected fix and files**: option and launch failures in `dispatch_pending` are treated like those of `prepare_sdk`
  (`not_started` + reason, or `fail_run`); `run` records the error of a cycle and continues, except for storage errors if
  that policy is kept.

#### F-ORCH-6 · The orchestrator's progress marks have done nothing since #277
**Class**: CONFIRMED · **Severity**: S3 · **Probability**: low · **Overlap**: none.
- **Invalid state/transition and broken invariant**: the watchdog measures progress with a counter that batch claim, host
  start, `handled`, recovery and the start and end of each tool operation no longer increment.
- **Evidence**: #277 (4f5e920d5) renamed the incrementer to `note_orchestrator_progress` and reused `orchestrator_progress()`
  as the read (`engine-kernel/src/domain_runtime.rs:126-132`). They still call the read as a statement:
  `domain_orchestrator.rs:529,551,698`, `recovery.rs:329`, `engine-kernel/src/domain_runtime/batch_operations.rs:26,50`.
  Only provider reports count (`domain_runtime.rs:442-447`) and commands that change something (`:1034-1036`).
- **Impact**: today it is covered by the provider stream events; a long tool call with no events is measured from the last
  event, not from its admission.
- **Deterministic test and ablation**: engine-kernel: `batch_operations::execute` with a job held by a channel; assert that
  `orchestrator_progress()` grew after admission and after the end. Today it does not grow. Ablation: going back to
  `note_...` makes it pass.
- **Expected fix and files**: call `note_orchestrator_progress()` at those points (or delete the calls if they should not
  count).

#### F-ORCH-7 · An accepted cancellation (HTTP 200) can be lost: `orchestrator.json` is written without a lock from two threads
**Class**: CONFIRMED (a concrete interleaving; a very narrow window) · **Severity**: S3 · **Probability**: very low · **Overlap**: none.
- **Invalid state/transition and broken invariant**: `cancel` answers 200 and the batch goes on; it breaks rule 5 ("An
  acknowledged command had an effect").
- **Evidence**: `mark_spawned` does `load` -> `run.spawned=true` -> `store` (`domain_orchestrator.rs:542-552`) from the
  provider thread; `cancel` (HTTP, `:365-375` through `decide` `:1001-1017`) does `load` -> `cancel_requested=true` ->
  `save`. Neither takes a lock. The `cancel` future only reads the file (`:557`).
- **Minimal sequence** (interleaving): A (`mark_spawned`) loads with `cancel_requested=false`; B (`cancel`) loads, sets true
  and saves; A saves its copy with false. The batch is not cancelled.
- **Impact**: the user's order is lost silently.
- **Deterministic test and ablation**: `Injected::fail_writes` with a hook that, on the first `runtime_write` after the claim,
  calls `executor.cancel(run)` and returns `Ok` (a barrier without sleeps); after the batch, `BatchEnd::Cancelled`. Today it
  ends `Handled`.
- **Expected fix and files**: a lock (a Mutex in `Orchestrator`) around each load-modify-save, or `cancel_requested` kept
  outside the file.

#### F-ORCH-8 · `create_task` carries no priority and automatic launch wakes in the same commit (issue #352)
**Class**: CONFIRMED (confirms #352 with paths) · **Severity**: S3 · **Probability**: medium · **Overlap**: issue #352 (new path evidence, same failure). #350 (moving tasks with create_task + cancel_task): no new evidence.
- **Evidence**:
  1. `Command::CreateTask` has no priority (`domain-rules/src/command.rs:147-163`).
  2. `set_priority` is another call (`cli-mcp/src/mcp.rs:580-583`).
  3. `auto_launch::run` wakes on every domain change (`engine-sdk/src/auto_launch.rs:32,48-52`) and launches by the current
     `task.priority` (`:111,150-151`).
- **Impact**: with a free slot, a task meant to be "low" starts before its priority arrives.
- **Deterministic test and ablation**: domain + `auto_launch::pass` with doubles; `CreateTask` and, before `SetPriority`, a
  `pass`: today the task is launched.
- **Expected fix and files**: priority in `CreateTask` (and in the tool).

### Hypotheses

#### H-ORCH-1 · Notices held without end (rule 10)
The cards to the user from a batch that ended "interrupted with confirmed effects" or "limit with effects" stay with
`ended=0` (handoff; `recovery.rs:100-115`) and their notice is held (`lib.rs:1576`) until a later batch ends `Handled`
(`domain_orchestrator.rs:692-696`). With the orchestrator failing continuously, the phone never receives that question.
Missing: check whether the app's inbox shows it anyway (then the damage is limited to the notice) and whether any other path
calls `finish_batch`.

#### H-ORCH-2 · Watchdog during a long tool
With requests without a task, a tool that lasts more than 5 min without stream events (for example `launch` creating a
worktree on a slow disk) makes the watchdog kill the host in the middle of the batch (`watchdog.rs:208-214`). Missing: check
whether any orchestrator tool can last that long.

#### H-ORCH-3 · `Answer` deadline with the orchestrator blocked
With the orchestrator Ambiguous (for example because of F-ORCH-1), the clock does not freeze (`obligation_watch.rs:122-126`):
after 20 min Alondra settles cases and withdraws questions that the orchestrator could not even see. This is the design
("busy or failing"), but combined with F-ORCH-1 it turns a restart into automatic decisions. It overlaps phase 4 of
the stalled-node recovery work (`AnswerSkipped`). Missing: an end-to-end run of F-ORCH-1 followed by the deadline.

## Agent hosts and providers (`PROV`)

Scope: the Node agent host (`agent-host/`: `persistent-core.ts`, `session.ts`, `host-session.ts`, `channel.ts`, `journal.ts`, `bootstrap.ts`, `persistent-main.ts`, `codex-main.ts`, `awake-clock.ts`, parts of `codex-session.ts` and `frame.ts`, plus `PERSISTENCE.md` and `README.md`). On the Rust side: `crates/provider-claude/src/wire.rs` (`Connection`), `adapter.rs` (prepare, send, reconcile), `normalizer.rs` (checkpoint, decisions, capabilities, `rate_limit_event`); `crates/provider-codex/src/normalizer.rs` (checkpoint and `fold`) and `ended.rs`; `crates/agent-protocol/src/correlation.rs`; `crates/agent-runtime/src/owned_host.rs` (`launch_through`, `claim`, `end_holders`, connection); `crates/agent-application/src/lib.rs` (`stage_observation`); `crates/store/src/controls.rs` and `commit_seals.rs`/`lib.rs` (`accept_observation`); in `crates/engine-sdk/src/sdk_owner.rs` only the parts that touch the host protocol (`take_host_into`, `lost`, `end_dead_host_claims`, `Driver::control`, `Driver::dispatch`, `step_once`, `settle_release`, `commit_observation`); `crates/core/src/accounts.rs`, `crates/core/src/usage.rs` (`record`, `ran_out`), `crates/daemon-engine/src/accounts.rs` (`check_current`, probe fence) and `usage_probe.rs` (`measure_account`, `record_limit`).

Vocabulary: the "host" is the Node process (`agent-host`) that holds the native session. The "owner" is the Rust side that drives it with a generation number (`owner_generation`). A "lane" is an authenticated connection to the host socket.

Covered by other areas and only linked here: per-task host ownership, the `Owner` thread loop and node close (ENGINE, M-ENGINE-4/5/10); `lifecycle_dispatches` and the observation journal in the store (STORE, M-STORE-4/5); cards and decision callbacks of the domain (`crates/domain`).

### Reviewed without finding

Read only superficially, with no finding to report; this is pending coverage, not a statement that nothing is wrong:
- `crates/provider-codex/src/adapter.rs` and `wire.rs`
- `codex-session.ts` outside the parts cited above
- `provider-conformance`
- `codex-protocol`
- the host model (`provider-claude/model/fake_claude_host.rs`)

### M-PROV-1 · Host folder and its `host.lock`
- **Kind**: implicit, on disk. **Defined in**: `crates/agent-runtime/src/owned_host.rs:196-328` (`launch_through`), `:855-891` (`claim`), `:896-932` (`end_holders`), `:935-959` (`connect_identity`); `agent-host/bootstrap.ts:56-108` (`loadBootstrap`, `assertBootstrap`, `prepareSocket`, `releaseBootstrap`); `agent-host/persistent-main.ts:24-32` and `codex-main.ts:21-29` ("unadoptable" watchdog); `crates/engine-sdk/src/host_folders.rs:128-151` (folder deletion).
- **Source of truth**: the `flock` on the inode of `state_dir/host.lock` (alive = someone holds it) and its content (bootstrap JSON). **Copies and derivatives**: the `stopped-by-alondra` mark, the `uncertain` file, the socket `s`, `outbox`/`outbox.next`. The daemon keeps no copy in SQLite: it decides by the lock.
- **States**:

| State | Encoding |
|---|---|
| Absent | no folder, or no `host.lock`, or it is empty (`claim` returns `Retained::Absent`, `:861-886`) |
| Preparing | the daemon holds the lock and writes the bootstrap (`:228-247`); `Preparing::held` protects the folder |
| Alive (Held) | the host process holds the lock inherited through stdin; `claim` returns `Retained::Held` (`:875-879`) |
| Ended | lock free and bootstrap not empty: `claim` takes it and returns `Retained::Ended(EndedHost)` with the lock in the daemon's hand (`Claimed`, `:848-853`) |
| Unadoptable | the lock inode lost its name (`nlink==0`): the host kills its group with SIGKILL (`persistent-main.ts:27-31`) |
| Deleted | `host_folders::remove` after `claim` returned something other than Held (`host_folders.rs:131-150`) |

- **Transitions**:

| From | Event [guard] | To | Effect |
|---|---|---|---|
| Absent | `launch_through` [requires `len()==0`, `:234-236`] | Preparing | bootstrap written |
| Preparing | `spawn` | Alive | the `File` is passed as stdin |
| Preparing | failure before launch | Absent | `set_len(0)` (`:318-324`) |
| Alive | process exits | Ended | the kernel releases the lock |
| Alive | `end_holders` | Ended | TERM to each holder, wait for exit without a clock, KILL the group (`:896-932`) |
| Ended | claimed by the daemon | Deleted, or relaunch in another folder | |

- **Rejected**: launching over a non-empty bootstrap (`"retained host bootstrap requires reconciliation"`, `:234-236`); changed inode, link count other than 1, different owner or mode (typed error `refused`); deleting while the lock is held (silently skipped).
- **Terminals**: Deleted. Ended is never reopened: a new host uses another folder instance (`sdk-<instance>`, `host_folders.rs:1-8`).
- **Invariants**: enforced: one holder per `flock`; the bootstrap is only written with the lock; the old socket is only removed once the lock is transferred (`bootstrap.ts:88-94`). Assumed: the bootstrap is never emptied after a successful launch, so Ended is always distinguishable from Absent.
- **Persistence and restart**: everything survives (disk). `adopt_all` ends the hosts of finished tasks (`sdk_owner.rs:1124-1150`) and adopts the rest.
- **Concurrency**: the `flock` is the barrier. `Claimed::drop` does an explicit `unlock` because a child forked by another thread could hold a copy of the descriptor (`owned_host.rs:845-853`).

### M-PROV-2 · Persistent host: session life inside the host process
- **Kind**: composite, implicit, in memory. **Defined in**: `agent-host/persistent-core.ts:64-446`.
- **Source of truth**: in-memory fields `session?`, `unavailable`, `stopping`, `stopPromise`, `sessionEnd`, plus `journal.recovering` (derived from disk on open, `journal.ts:61`).
- **States**:

| State | Encoding |
|---|---|
| No session | `session === undefined`, `!unavailable`, `!stopping` |
| Recovering | `journal.recovering` (the outbox already had commands or an ACK, or `uncertain` exists): every new order is rejected with `host_reconciliation_required` (`:186-188`). It is `readonly` and never exits |
| With session | `session` set (by `start`, by `offer` if `journal_base` is set, or by a control if `opens_on_control`, `:200-217`, `:354-358`) |
| Unavailable | `unavailable = true`: storage failure (`storageUncertain`, `:377-381`) or session ended with `endsWithSession`/`endsWithFailedSession` (`:313-316`) |
| Waiting for final ACK | `sessionEnd` set: the session finished and the ACK of the last cursor is awaited, with 5 s/6 s deadlines (1 s/2 s if it failed) (`:317-334`) |
| Stopping | `stopping = true`, shared `stopPromise` (`:419-445`) |

- **Transitions**: No session to With session (first valid order). With session to Waiting for final ACK to Stopping (native end, only with the Codex or failure hooks). Any to Stopping (`SIGTERM`, `SIGINT`, `stop()`). Any to Unavailable (journal I/O).
- **Rejected**: `start` with a session already open, with another `cwd`, or with `journal_base` set (`:201`) throws an **untyped exception**, which becomes `sdk_action_uncertain` and a destroyed socket (`:303-305`, `:151`). `offer`/`interrupt`/`close` without a session behave the same (`:214`, `:228`, `:237`). Orchestrator: any action except `start`/`interrupt`/`close` gets `rejected single_turn` (`:189-191`).
- **Terminals**: Stopping (the process exits). Unavailable has no exit: it blocks effects until stop.
- **Persistence and restart**: a host restarted over the same outbox stays in Recovering forever. In practice every new host gets another folder, so this does not happen.
- **Concurrency**: one JS thread. Orders are serialized in the `operations` promise (`:149-153`); ACKs and `attach` are not (processed on arrival).

### M-PROV-3 · Owner lane and generation
- **Kind**: explicit in the host, mirrored in Rust. **Defined in**: host `persistent-core.ts:114-169` (`current`, `connection`, `receive`, `attach`); Rust `crates/provider-claude/src/wire.rs:96-185` (`Connection::attach`), `:426-438` (`check_owner`); the generation is reserved in the store (`reserve_owner`, see M-ENGINE-4).
- **Source of truth**: `journal.ownerGeneration` (fsynced `attached` record, `journal.ts:130-132`) and `this.owner` (live lane). In Rust: `Connection.failed` and `pending_attach.owner_generation`.
- **States per lane**: Connected (hello and challenge sent, fresh `nonce`) to Owner (attach with generation greater than the journal's and a valid HMAC proof) to Revoked (socket closed, or another lane with a higher generation, `:165-167`). In Rust: Healthy to Failed (`failed = true` after any error in `submit`, `reconcile`, `next` or `acknowledge`; sticky, `wire.rs:192-196`).
- **Guards**: `current(lane)` before each effect and after each `await` (`:114-117`); the ACK requires the generation, `cursor <= sentCursor` and `cursor >= acknowledged` (`:137`). In Rust every operation goes through `check_owner`: a failed connection prepares and sends nothing (`wire.rs:317-329`).
- **Rejected**: attach with an equal or lower generation, spent nonce or false proof destroys the socket with diagnostic `owner_message_rejected`. Any frame other than `attach` before becoming owner does the same.
- **Terminals**: Revoked. A reconnection needs a new generation (`PERSISTENCE.md`, paragraph 3).
- **Persistence and restart**: on daemon restart the host keeps running; the new daemon reserves a new generation and attaches.

### M-PROV-4 · Order and its receipt in the host journal
- **Kind**: explicit. **Defined in**: `agent-host/journal.ts:79-157` (`validateRecord`, `apply`, `settle`); `persistent-core.ts:170-307` (`command`), `:359-362` (`receipt`), `:382-411` (`flush`). Rust mirror: `crates/agent-protocol/src/correlation.rs` (`CommandRegistry`), `wire.rs:199-313` (`submit_inner`, `reconcile_inner`), and in the store the `lifecycle_dispatches` rows (M-STORE-4) and `lifecycle_controls` (M-PROV-6).
- **States per `command_id`**:

| State | Where |
|---|---|
| Unknown | in neither `commands` nor `settled` |
| Admitted | `command` record plus `accepted/admitted` receipt, fsynced before the effect (`:183-185`) |
| Offered / Applied | receipt `accepted/offered` (inputs) or `accepted/applied` (controls) |
| Rejected | `rejected` receipt with a `code` |
| Settled (in memory) | final plus a later frame from the owner: leaves the journal, keeps `digest` and receipt in `settled` (`journal.ts:150-157`) |

- **Allowed transitions** (`journal.ts:88-93`): Admitted to Offered/Applied; Admitted to Rejected; phases only move forward.
- **Rejected**: Rejected to anything else (`"Rejected receipt is terminal"`), Offered/Applied to Rejected (`"Possible handoff cannot become rejected"`), phase going back; all by typed error.
- **Admitted forever**: if the effect throws something that is not `InputNotOffered` or `ControlNotApplied/native_code_cells_active`, the host writes no final receipt, destroys the socket and leaves the order Admitted (`:303-305`, `:151`). Resending the same bytes returns that admitted receipt, which Rust reads as `Uncertain` (`wire.rs:304`), never as permission to retry.
- **Rejections before registering**: capacity (256 orders) or an expired deadline (`awake.passed`) throw before `persist` (`:182`): no trace is left and the socket is destroyed (F-PROV-2).
- **Idempotency**: same `command_id` and same digest returns the retained receipt; a different digest is a conflict and the socket is destroyed (`:172-174`). Rust never chains orders: `submit` waits for the final receipt before returning (`wire.rs:226-256`), so the assumption in `settle()` ("the owner sends nothing after an order before saving its result") holds while there is a single owner.
- **Persistence and restart**: the host stays alive, so `commands`/`settled` remain. When the daemon restarts, it reconciles `claimed` rows by resending exact bytes (`sdk_owner.rs:971-982`). For an ended host, its outbox is read as `EndedJournal` (`provider-codex/src/ended.rs`, same in Claude) and yields `Offered/Refused/AdmittedOnly/Absent`.

### M-PROV-5 · Event outbox, cursor and ACK
- **Kind**: explicit, composite with M-STORE-5. **Defined in**: host `journal.ts:44-63`, `:126-141`, `:158-177`; `persistent-core.ts:363-373` (`observe`), `:382-411` (`flush`); Rust `wire.rs:342-415` (`next_inner`, `acknowledge`), `:450-466` (`LaneCursor`); store `crates/store/src/lib.rs:1092-1134` (`accept_observation`).
- **Source of truth**: the host outbox (events with `cursor` = previous + 1, starting at `journal_base` in a continued host) and, in SQLite, `provider_observation_heads` plus the normalization checkpoint.
- **States per event**: Persisted, then Sent to the lane (`lane.sentCursor`), then Delivered to Rust (`LaneCursor.delivered`), then Committed in SQL (observation, checkpoint and facts in one transaction), then ACK sent, then ACK persisted in the host (the event leaves memory; the file on compaction).
- **Guards**: the first event of a new lane must have `cursor <= committed+1` and the following ones must be contiguous (`wire.rs:455-465`). The store accepts only head+1 and treats an already stored cursor with the same digest as `Duplicate` (no facts) and with another digest as `ObservationConflict`. The ACK can only cover what was delivered (`wire.rs:393-395`).
- **Redelivery after reconnection**: the host resends from `journal.acknowledged` (`:121`); what Rust already committed arrives as a duplicate and repeats no facts. There are no gaps and no undetected duplicates.
- **Invariants**: capacity is 64 MiB of un-ACKed evidence; beyond that the host marks `uncertain` and stops admitting effects (`journal.ts:73`, `persistent-core.ts:374-381`).
- **Ordering between receipts and events**: on each turn `flush` sends receipts first and then events (`:391-406`). Rust queues events that arrive while waiting for a receipt (`wire.rs:252`) and ignores stray receipts in `next` (`:351-357`). Read end to end: no consumer depends on receipt/event order.

### M-PROV-6 · Permission decision
- **Kind**: composite (host, normalizer, store, domain). **Defined in**:
  - Claude host `agent-host/session.ts:108-186` (`PermissionCallbacks`); Codex host `codex-session.ts` (`asked`, `answered`, plans);
  - Claude normalizer `crates/provider-claude/src/normalizer.rs:352-382` (`Checkpoint.asked`) and Codex `crates/provider-codex/src/normalizer.rs:140-153`;
  - store `crates/store/src/controls.rs:393-466` (`put_decision_request`, `stage_response`), `:473-519` (`claim_control`), `:582-596` (`record_control_receipt`), `:599-614` (`unresolved_controls`);
  - domain: run callbacks, emptied when the host ends (`crates/domain/src/lifecycle/observations.rs:498`).
- **Source of truth**: the pending promise inside the host (the only thing that can unblock Claude). **Copies and derivatives**: event `decision_requested` in the outbox; `asked` in the checkpoint; row `lifecycle_decision_requests` (epoch = event cursor); domain callback and card; row `lifecycle_controls` of the answer.
- **States in the host**: Asking (no epoch), then Pending (epoch = persisted cursor), then Settled `answered | cancelled` (event `decision_settled`). With no persistable epoch the request is denied at once (`session.ts:163`).
- **States of the answer** (`lifecycle_controls`, trigger `crates/store/src/controls.sql:54-60`): `prepared -> claimed -> handed | rejected | uncertain`; `prepared -> revoked` (if the callback is no longer `Answered`, `controls.rs:330-338`).
- **Terminals**: `uncertain` is terminal in the table.
- **Double-answer guards**: one row per callback (`control_id` = callback, `controls.rs:449-458`). In the host, `respond` requires the same `request_id`, `epoch`, `kind`, `tool_use_id` and an identical `asked` (`session.ts:174-181`); the second answer finds the entry deleted and gets `rejected stale_request`. Checked: a decision cannot be applied twice.
- **Persistence and restart**: if the host dies, the domain empties the callbacks when it records `HostEnded`; a resumed Claude does not keep the question (see F-PROV-3 for what remains in the checkpoint). An `uncertain` answer is never reconciled (H-PROV-1).

### M-PROV-7 · Native session inside the host
- **Kind**: explicit in code, in memory.
- **Claude** (`agent-host/session.ts:189-386`): `turnOpen`, set `inputs` (sends waiting for echo, max 64), `closing`, `seenResult`, `child`/`childEnded`. States: Opening (no `init`), Idle, Turn open (offer or root `assistant`), Idle (root `result`), Closing (`close()`), Finished (`finished` waits for the real exit of the native process, `:362-370`). `forceStop` = `close` + TERM + KILL with 2 s between steps (`:301-309`). Orchestrator: closes input after the first `result` with no pending sends (`:357`).
- **Codex** (`agent-host/codex-session.ts:66-560`): `opening` (`initialize -> account -> thread_resume | thread_start`), `thread`, `turn`, `queue` (max 64), `starting`, `pumping`, `closed`/`closing`, `asked`/`answered` (plans), child states. Opening deadline is 90 s (`:14`, `:164`). A queued input starts only with no open turn (`pump`, `:424-458`).
- **Source of truth**: the native process; the host only carries a technical mirror.
- **Persistence and restart**: nothing survives a host restart (the session resumes through `resume`/`thread_resume`).

### M-PROV-8 · Life of each input
- **Kind**: composite (host, dispatch, normalizer, domain). **Defined in**: host `persistent-core.ts:200-218` (`start`/`offer`), `session.ts:281-289`, `codex-session.ts:416-458`; Rust `wire.rs:199-257`; dispatch M-STORE-4; normalizers (`taken`/`finished` in Claude, `input_starts`/`accepted` in Codex).
- **States (observable)**:

| State | Evidence |
|---|---|
| Prepared | row `prepared`, bytes not yet recorded |
| Claimed | row `claimed` plus exact bytes (`record_command`) |
| Rejected before offer | receipt `rejected` or capability not announced: `Handoff::RefusedBeforeOffer` |
| Offered | receipt `offered`: Claude put it in the SDK `Input` queue; Codex in its `queue` (for a worker with an open thread, **only queued**, `codex-session.ts:416-422`) |
| Uncertain | `submit` failed after it could have written: `Handoff::Uncertain` (`sdk_owner.rs:1884`) |
| Starting (Codex) | event `alondra/inputStarting` (`input_starts`) |
| Held | `InputHeld` (Codex `inputAccepted`) |
| Taken | `TurnStarted`/`InputConsumed` |
| Dropped | `InputDropped` (Codex `inputRefused nativeSubmitted:false`, `inputRejected`, `turn/completed` without the message; Claude `result` that names it without taking it) |

- **Double delivery**: the Claude host rejects a repeated `input_id` while it is pending (`session.ts:285`); a resend from the domain uses a new `input_id` (`HandoffResent`). Across different hosts (continuation) the pending set starts empty: what prevents double delivery is the domain's decision on a doubtful handoff (card `resend_doubtful`), not the host.
- **Zero delivery**: "Offered" does not prove the native process received it (in-memory queue of the host). If the host dies first, the ended journal says `Offered` and the domain decides on recording `HostEnded` (`resume`). This gap is exactly point 10 of the provider adapters work (phase 3) (`accepted | started | discarded | refused` confirmed by the provider); it is not repeated as a finding.

### M-PROV-9 · Claude normalizer checkpoint
- **Kind**: explicit, per run. **Defined in**: `crates/provider-claude/src/normalizer.rs:44-104` (`Checkpoint`), `:202-293` (`normalize`).
- **Source of truth**: `provider_normalization_checkpoints` (one row per run, cursor = the observation's). It is rebuilt observation by observation; there is no cache.
- **State**: `turn` (`LogicalTurn`), `work`, `woken`, `taken`, `finished`, `asked`, `isolated`, `pr_commands`, `pr_close_commands`, `mcp_calls`, `listed`, `running`, `limit_resets_at`, `seen_tasks`, `initialized`.
- **Special transitions**: first cursor of a continued host: `forget_ended_host` clears `turn`, `work`, `woken`, `taken`, `finished`, `listed`, `running` (`:94-103`, `:242-244`), but **not** `asked`, `isolated`, `pr_commands`, `pr_close_commands`, `mcp_calls`, `limit_resets_at` (F-PROV-3). Last events of an ended host (`host_ended`): native exit and iterator end produce no facts (`:249-253`).
- **Rejected**: an `Unsupported`/`InvalidObservation` error from the fold restores the previous checkpoint and leaves the observation **without facts and without a record** (`:283-289`); issue #328 is open. An error of another kind propagates and the owner discards the observation with a reason (`sdk_owner.rs:2454-2459`).
- **Invariants**: accepting facts from another run is impossible by construction. The event must match task, node, conversation, instance, generation, host and fence (`:224-235`), the store requires the run to be the current one (`lib.rs:1100`), and the previous checkpoint must be at `cursor-1`.

### M-PROV-10 · Codex normalizer checkpoint
- **Kind**: explicit, per run. **Defined in**: `crates/provider-codex/src/normalizer.rs:18-63`, `:120-188`.
- **State**: `thread`, `process`, `turn`, `idled`, `work`, `asked`, `accepted`, `accepted_plans`, `input_starts`, `continuation(s)`, `child_*`, `model*`, `blocked_policy`.
- **Special transitions**: first cursor of a continued host clears turn, work, starts and children (`:136`); `alondra/threadReady` with another `processGeneration` also clears `asked`, `accepted` and `accepted_plans` (`:166-170`).
- **Rejected**: fold errors propagate and the owner discards the observation with a record (`discarded_observations`).
- **Invariants**: facts from another run are refused as in M-PROV-9; it also ignores events of a thread that is not the root (`fold`, `:192`).

### M-PROV-11 · Codex host process and native life
- **Kind**: explicit. **Defined in**: `agent-host/README.md` ("Codex process supervision"), `codex-main.ts:10-14` (`endsWithSession`, `endHostGroup`), `persistent-core.ts:313-344`; Rust `CodexWorker::native_liveness()` (`NotStarted | Alive(Member) | Lost`).
- **States**: No app-server (opens on first input), Alive, Lost. The host ends when its native session ends, waits for the ACK of the last cursor (5 s without progress, 6 s watchdog) and kills its group with the lock still held.
- **Overlap**: the Codex recovery work (phase 2), points 9-11 ("not started yet" versus "lost it", typed exit, adopted host pid).

### M-PROV-12 · State of each account
- **Kind**: derived, snapshot. **Defined in**: `crates/core/src/accounts.rs:541-561` (`AccountState`: `Unchecked | SignedIn | SignedOut | Unavailable | Disabled`), `:453-536` (`AccountsConfig::changed`); produced in `crates/daemon-engine/src/accounts.rs:134-176` (Claude), `:522-612` (`check_current`, Codex) and `:416-418`.
- **Source of truth**: `options.toml` (`enabled`, order, policy) plus the native probe. The domain stores the snapshot (`RecordAccounts`) and each probe replaces it whole.
- **Transitions**: any state to any other on each probe; `Disabled` if `enabled=false`; `Unavailable` if the probe fails (`:166`, `:559`); `Unchecked` at startup or on re-enable (`:895-897`).
- **Concurrency (fence)**: the probe commit compares `accounts`, configuration versions and `options.toml` with the snapshot taken before (`commit_probe`, `:333-345`); if they changed, `stale_probe` and it is repeated. It does **not** include usage (`account_usage`), see F-PROV-1.
- **Missing**: no native error of a node (401, "login expired") changes the state; that is the scope of the expired provider login work.

### M-PROV-13 · Usage and limit of each account
- **Kind**: implicit, derived from readings. **Defined in**: `crates/core/src/usage.rs:198-300` (`AccountUsage`, `ran_out`, `exhausted_until`), `:329-358` (`record`); `crates/domain-rules/src/rules.rs:3442-3453` (`RecordAccountUsage`).
- **States per window**: Unmeasured, then With margin (`used < FULL`, not `exhausted`), then Exhausted until `resets_at` (`exhausted` or `used == FULL`, with a future reset), then Exhausted with no known time, then With margin again (`resets_at` passes, another reading arrives, or in Codex a working turn, `native_turn_worked`).
- **Writers**: `usage_probe::measure_account` (full reading, always `exhausted:false`, `usage_probe.rs:100-121`), `usage_probe::record_limit` (orchestrator), turn observations (`store-sqlite/src/owner.rs:301-323`, `:486-497`), `accounts::check_current` (Codex).
- **Invariants**: assumed and not enforced: an older reading does not overwrite a newer one. `record` replaces the window of the same dimension without looking at `measured_at`, except the native-limit mark of Codex (`usage.rs:331-333`). See F-PROV-1.

### M-PROV-14 · Orchestrator batch over the host
- **Kind**: explicit, bounded. **Defined in**: `persistent-core.ts:189-191`, `session.ts:212-226`, `:357`; Codex `codex-session.ts` (`if (this.config.orchestrator) this.close()` when the turn ends).
- **States**: one turn per host; any later input is rejected with `single_turn`; after the `result` with no pending sends the input is closed and the host ends. The batch machine itself belongs to ORCH (M-ORCH-1/3).

### Findings

#### F-PROV-1 · A usage reading measured before a limit erases it when it arrives later: the account reads as having margin again
**Class**: CONFIRMED · **Severity**: S2 · **Probability**: low to medium (each account is probed every 15 min, `usage_probe.rs:28`; a probe takes seconds and up to 60 s, `:29`; a limit arriving inside that window is enough) · **Overlap**: no open task. Issues #396/#330/#342 are about how limits are shown, not their order. `git log` of `usage.rs` and `usage_probe.rs`: no later fixes. The evidence is new.
- **Invalid state/transition and broken invariant**: M-PROV-13 in "With margin" after the account itself said "Exhausted", because `RecordAccountUsage` applies readings in arrival order, not measurement order. This breaks the assumed invariant "an older reading does not overwrite a newer one".
- **Evidence**:
  1. `measure_account` takes `measured_at` before launching the probe (`crates/daemon-engine/src/usage_probe.rs:333-334`) and the reading is always `complete: true` with `exhausted: false` (`:108-119`).
  2. Meanwhile a node of that account receives `rate_limit_event` with `status: rejected`: the normalizer produces a window `exhausted: true` (`crates/provider-claude/src/normalizer.rs:1104-1116`) which the store records with `at = now` (`crates/store-sqlite/src/owner.rs:486-495`). In the orchestrator, `record_limit` does the same with `used: FULL, exhausted: true` (`crates/engine-orchestrator/src/orchestrator_provider.rs:80-93`).
  3. The probe finishes and publishes with `at = measured_at` (`usage_probe.rs:336-337`). The `commit_probe` fence compares accounts, versions and `options.toml`, not `account_usage` (`crates/daemon-engine/src/accounts.rs:339-345`), so the commit passes.
  4. `record` replaces the window of the same dimension without looking at `measured_at` (`crates/core/src/usage.rs:334-342`); it only protects the native-limit mark of Codex (`:331-333`). `RecordAccountUsage` applies it as is (`crates/domain-rules/src/rules.rs:3442-3453`).
  5. `plan_exhausted`/`plan_exhausted_until` no longer see the exhausted window (`usage.rs:258-300`).
- **Minimal sequence**:
  1. The probe of account A starts at t0.
  2. At t1 > t0 a turn of A is rejected by a limit and the Session window is recorded as exhausted until R.
  3. At t2 the probe publishes Session `used = 0.97`, `exhausted:false`, measured at t0.
  4. A reads as having margin until the next rejection or the next probe (15 min).
  - Variant with the same origin: events of an ended host are committed on adoption, maybe hours later (`sdk_owner.rs:999-1007`), and their usage readings are recorded with `at = now` (`owner.rs:492`): an old reading from the journal overwrites a recent one.
- **Impact**: `out_of_quota` stops moving the task (`crates/domain-rules/src/task_selection.rs:82-88`), selection picks A again for new starts, and `usage_resets_by_task` loses the return time of held tasks that have none of their own (`crates/domain-rules/src/claude_access.rs:221-229`). Work starts, hits the limit and waits again; nothing is corrupted.
- **Deterministic test and ablation**: a pure domain test on `DomainState`: `RecordAccountUsage` with Session exhausted at `at = 200`, then a complete `RecordAccountUsage` with Session `used 9700`, `exhausted:false` and `at = 100`; assert `plan_exhausted(150)` is true. Today it fails (returns false). Ablation: remove the `measured_at` comparison in `record`.
- **Expected fix and files**: `record` ignores, window by window, a reading whose `at` is lower than the stored `measured_at` (and the probe fence includes the `account_usage` revision of that account); readings from an ended journal carry the event time, not the commit time. Files: `crates/core/src/usage.rs`, `crates/daemon-engine/src/usage_probe.rs`, `crates/store-sqlite/src/owner.rs`.

#### F-PROV-2 · The reconciliation resend of an order the host never had, past its deadline, is rejected by destroying the socket: adoption fails and a live host is taken for lost
**Class**: CONFIRMED (path read end to end) · **Severity**: S2 · **Probability**: low. It needs a `claimed` row with recorded bytes that the host did not admit (the daemon dies between the commit of `record_command`/`record_control` and the socket write) and an adoption more than `io_timeout` (120 s awake) after the order was prepared (daemon stopped or slow update while the host stays alive, which is by design) · **Overlap**: the Codex recovery work (phase 2), point 8 ("Rejections of a host order come with a typed code (`rejected` plus `code`), not as `sdk_action_uncertain`"). That point is about the rejection of actions; this path (admission rejection before registering, during a reconciliation) is **new** evidence for that task, with its consequence on adoption. #481 already touched the deadline for Mac sleep, not reconciliation.
- **Invalid state/transition and broken invariant**: M-PROV-4, "Unknown" plus resend: the host answers neither `rejected` nor a receipt, it cuts the lane. A reconciliation is by construction later than the order's deadline, and for an order the journal does not have, the right answer is positive: "it was never offered".
- **Evidence**:
  1. `command()` looks for the `command_id` in `commands`/`settled` (`agent-host/persistent-core.ts:172`); if absent, it checks capacity and deadline **before** registering and throws `"Command admission refused"` (`:182`).
  2. The order queue catches the exception: `socket.destroy()` plus diagnostic `owner_command_uncertain` (`:150-152`). No receipt.
  3. Rust, in `reconcile_inner`, waits for a receipt of that order (`crates/provider-claude/src/wire.rs:293-308`) and gets the disconnect: `Err`, `failed = true`.
  4. `take_host_into` propagates the error with `?` (`crates/engine-sdk/src/sdk_owner.rs:971-976`); the `end_refused` path only covers a failure of `adopt_owner` (`:939-948`), not of reconciliation.
  5. `adopt_all` to `not_taken` to `lost` (`sdk_owner.rs:1103-1116`, `:1155-1161`, `:1204-1232`): `end_dead_host_claims` sees pending claims and ends the live host (`:1240-1254`), and `RecordHostLost` raises the lost-host card.
  6. The deadline is in the exact bytes (`deadline_ms = now + io_timeout`, `crates/provider-claude/src/adapter.rs:681-701`), so retrying with the same bytes always fails the same way.
- **Minimal sequence**:
  1. The owner records order C (`claimed` plus bytes).
  2. The daemon dies before writing it.
  3. The host stays alive.
  4. The daemon returns 3 min later.
  5. Adoption reconciles C, the host cuts the socket, the host is declared lost, the running turn is cut with TERM, and a card goes to the user.
- **Impact**: a node that was working fine loses its running turn and the user sees "host lost" where a clean adoption was due; "Retry" relaunches with the resumed session. Nothing is lost permanently.
- **Deterministic test and ablation**: a host test (`agent-host`, no real Claude: the `SessionFactory` double already used by `ownership.test.mjs`/`reconcile.test.mjs`) with an injected `AwakeClock`: attach, send an order with an already passed `deadline_ms` and a new `command_id`, assert a `rejected` receipt with a typed code arrives and the socket stays open. Today the socket closes without a receipt. In Rust, with the `ModelHosts` double, a `take_host_into` case whose reconciliation receives that rejection must resolve the row as `rejected` and adopt. Ablation: throw again before `persist`.
- **Expected fix and files**: the host registers the order and answers `rejected` with `code: expired` (or `capacity`) without effect; Rust reads it as `RefusedBeforeOffer` and the input returns to the queue. Files: `agent-host/persistent-core.ts`, `crates/provider-claude/src/wire.rs` (and its twin in `provider-codex`).

#### F-PROV-3 · The Claude checkpoint carries open decisions and calls over from the host that died; when full, a new decision is silently dropped
**Class**: CONFIRMED (mechanism) · **Severity**: S3 as reached today; S1 if the cap is reached (Claude waits for a permission nobody will see) · **Probability**: very low for the cap (64 dead hosts without an orderly close, each with an open decision, in the same run generation) · **Overlap**: issue #328 ("Log fold errors in the Claude normalizer with cursor and error") covers only logging of the swallowed error. the stalled-node recovery work (phase 3) (`DecisionGone`) looks at the decision from the domain, which is already cleaned. New evidence for both.
- **Invalid state/transition and broken invariant**: M-PROV-9 with entries in `asked` (and in `mcp_calls`, `pr_commands`, `pr_close_commands`, `isolated`) from a Claude process that no longer exists.
- **Evidence**:
  1. `forget_ended_host` clears turn, work and entries, not `asked` nor the call sets (`crates/provider-claude/src/normalizer.rs:94-103`).
  2. A host that dies without closing (SIGKILL of the group when its lock loses its name, `agent-host/persistent-main.ts:27-31`; a Mac hang or reboot) emits no `decision_settled`: only `PermissionCallbacks.close()` does, inside an orderly close (`session.ts:182-185`). The domain does empty its callbacks on recording `HostEnded` (`crates/domain/src/lifecycle/observations.rs:498`); the checkpoint does not.
  3. `DecisionRequested` requires `asked.len() < 64` and a non-repeated `request_id` (`normalizer.rs:354-356`); otherwise `InvalidObservation`, which `normalize` swallows by restoring the checkpoint and producing no facts (`:283-286`): no `Asked`, no card, and the host keeps waiting for the answer.
  4. For `mcp_calls`/`pr_commands` the cap makes new calls not remembered (`:938`, `:989-996`): `McpToolFailed` and `PullRequestOpened` of later commands are lost.
  5. Contrast: Codex does clear `asked`, `accepted` and `accepted_plans` when the process changes (`crates/provider-codex/src/normalizer.rs:166-170`).
- **Minimal sequence**:
  1. Claude asks for permission (epoch E, `asked = {E}`).
  2. The host dies without an orderly close.
  3. A continued host resumes the session: `forget_ended_host` leaves `asked = {E}`.
  4. Repeat up to 64.
  5. The next request is silently dropped and the turn stays open.
  - Variant (unverified): if the SDK repeated `request_id` across processes, step 5 would happen at the first repetition. Not checked: it would need reading the SDK, outside the permitted scope (`node_modules`).
- **Impact**: see severity; today a stale entry occupies a slot, at the cap a decision is lost and Claude waits forever.
- **Deterministic test and ablation**: a pure normalizer test (in-memory checkpoint like those at `normalizer.rs:1961-1985`): `decision_requested` at cursor 3, then `first_cursor = 4`, and assert `asked` is empty after the first event of the new host. Today it stays `{3}`. Ablation: remove the added `clear`.
- **Expected fix and files**: `forget_ended_host` also empties `asked`, `mcp_calls`, `pr_commands`, `pr_close_commands` and `isolated` (the requests and calls died with their Claude), and a reached cap is recorded as a discard with a reason instead of being swallowed (`normalizer.rs:283-286`). File: `crates/provider-claude/src/normalizer.rs`.

#### F-PROV-4 · A continued host rejects `start` without a type (known overlap)
**Class**: CONFIRMED · **Severity**: S2 · **Probability**: medium when there is a continuation with the launch permission pending · **Overlap**: the Codex recovery work (phase 2), point 7, with exactly this evidence. **Not new**; listed so the inventory is complete. It adds no work outside that task.
- **Invalid state/transition and broken invariant**: M-PROV-2, `start` on a host with `journal_base` set is refused with an untyped exception.
- **Evidence**:
  1. `persistent-core.ts:201` throws `"Owned session configuration refused"` if `journal_base` is set; it ends as `sdk_action_uncertain` and a destroyed socket (`:303-305`, `:151`).
  2. Rust sends `Action::Start` whenever the operation is `Launch` (`adapter.rs:917-930`).

### Hypotheses

#### H-PROV-1 · A response to a decision that ends `uncertain` is never reconciled
What is known: `lifecycle_controls` makes `uncertain` terminal (`crates/store/src/controls.sql:54-60`); `unresolved_controls` only resends `claimed` rows (`crates/store/src/controls.rs:591-606`); nothing in the domain or the engine reacts to an `uncertain` control. If the host never applied the answer, Claude keeps waiting for the permission (the promise lives in the host, M-PROV-6) while the domain has the callback `Answered`: an open turn with no exit (S1).

What is missing: a realistic trigger where the host does not apply the answer. Ruled out: lane revocation between registration and effect (there is no event boundary between `persist` and `session.respond`, `persistent-core.ts:183-225`), and sending over an already failed connection (`prepare_control` fails before recording bytes, `wire.rs:317-329`, and the row stays in `unrecorded_controls`). Remaining: admission rejection by deadline when the host's `operations` queue is busy for more than `io_timeout` with an order from an earlier lane (for example two `Owner`s over the same host, H-ENGINE-1), or a partial `write`. With a trigger, the test would be a `take_host_into` case with the host double leaving the answer pending and an `uncertain` row.

Overlap: the provider adapters work (phase 3) point 9 (`response: live | message | not_resumable`) and the stalled-node recovery work (phase 2) (`host_state` with open decisions, which would allow detecting the contradiction "the host waits, the domain considers it answered"). Neither names this case.

#### H-PROV-2 · An input "offered" to Codex is only queued in the host
What is known: for a worker with an open thread, `offer` puts the input in `queue` and returns without waiting for `turn/start` (`codex-session.ts:416-422`), and the host writes the `offered` receipt. If the host dies before `pump`, its journal says `Offered`; in the continued host `first_cursor` empties `input_starts` without producing `InputDropped` (`provider-codex/src/normalizer.rs:136`).

What is missing: how the domain resolves that input on `HostEnded` (`resume`, `observations.rs:492`) when there are two or more such inputs. This is point 10 of the provider adapters work (phase 3); it is not pursued further.

## Daemon devices, pairing, self-update and relay (`SERVER`)

Scope: audited at commit 1567967b9. The devices part covers the daemon-side device lifecycle (approval, direct
pairing, local terminal pairing, relay enrollment, revocation, expiry, iroh, push recipients) and the daemon's
self-update and relaunch. It excludes `operation_journal`, `views/*` and `domain_delivery` apart from push
distribution. The relay part covers `crates/relay` (pairing mailbox, device registry, push and Live Activity
registrations, web login and sessions, invitations, accounts, account outbox, revocation tombstones, waitlist,
admin idempotency, JWKS cache). The signed-request verifier (`alondra_e2e::HttpChallengeVerifier`) appears only as
machine M-SERVER-D11, and envelope cryptography is out of scope. Overlap with open tasks could not be checked
(`.scratch/audit/the plans of related work` and the open issues did not exist); `git log` of each file shows no later
fix for any finding. Relay machine ids `M-SERVER-R<n>` are kept as in the relay report.

### Part 1: daemon devices, pairing, self-update

### M-SERVER-D1 · Device approval
- **Kind**: implicit. **Defined in**: `crates/daemon-support/src/auth.rs:36-44` (`Device`), `:96-121`
  (`PairedDevices`), `:129-150` (`Auth`), `:183-202` (`approved_devices` + `APPROVED` cache), `:264-344` (`save`,
  `pair`, `approve_service`), `:375-388` (`revoke`), `:402-454` (`verify_at`), `:475-495` (`guard`).
- **Source of truth**: `devices.json` (list of `Device`). **Copies and derivatives**: `Auth.devices` in memory (one
  instance per daemon, created at `domain_bootstrap.rs:80`); the static `APPROVED` cache keyed by file identity
  (dev/ino/size/mtime/ctime); in each `Service`, the keys pinned in `service.toml` (`Service.devices`). Short-lived
  copies: `Auth::open(home)` at `service.rs:1871`, `:1893` and `views/action_results.rs:27`.
- **States**: `Absent` | `Approved(id, key)`. Relevant product: `Approved` × `Pinned in service.toml` × `Relay
  enrollment` (M-SERVER-D5) × `Tombstone` (M-SERVER-D2).
- **Events**: `pair(code)` (single-use code, `auth.rs:279`); `approve_service(id, key)` (`:310`; guards: id shape,
  canonical key, no tombstone, same id means same key); `revoke(id)` (`:375`). No external writers while the daemon
  runs (the CLI recovery works on the daemon beforehand: `cli/src/service_setup/recovery.rs:2`).

| from | event [guard] | to | effect |
| --- | --- | --- | --- |
| Absent | `pair` | Approved (random id) | saves `devices.json` |
| Absent | `approve_service` [no tombstone] | Approved | saves `devices.json` |
| Approved | `approve_service` [same key] | Approved | nothing written |
| Approved | `approve_service` [other key] | Approved | error |
| Approved | `revoke` | Absent | saves `devices.json` |

- **Rejected**: typed errors `ServiceApprovalError::{Revoked, Refused}`; 401 in `guard`.
- **Invariants enforced**: writer holds `ApprovalLease::exclusive` plus the mutex; save before replacing memory;
  `verify_at` checks with `Arc::ptr_eq` that the approval did not change during verification (`:434`); handlers
  re-read the file with `signed_device` (`domain_bootstrap.rs:307`).
- **Persistence and restart**: tmp+rename (no fsync). A restart re-reads the file.
- **Concurrency**: lease (flock across processes), then the `devices` mutex; `verify_at` takes only the mutex. Lock
  order (lease before mutex) is consistent in `pair`, `approve_service` and `revoke`.

### M-SERVER-D2 · Revocation tombstones
- **Kind**: implicit. **Defined in**: `crates/daemon-support/src/revoked_devices.rs:13-110`; writer
  `service.rs:924-930`.
- **Source of truth**: `revoked-devices.json` (FIFO of at most 64). **Copies**: in-memory `Service.revoked`;
  `approve_service` reads the file from scratch (`auth.rs:324`).
- **States**: `No tombstone` | `Tombstone(id, keys, revoked_at)`. Terminal except for FIFO eviction (`:63-64`).
- **Uses**: refuses approving the same id again (`auth.rs:324`); answers a sealed «device_revoked» to direct/iroh
  requests (`service.rs:1413-1440`); accepts the iroh hello of a revoked device so it can be told
  (`service.rs:1015`).
- **Persistence**: tmp+rename.

### M-SERVER-D3 · Direct pairing request
- **Kind**: explicit. **Defined in**: `crates/daemon-server/src/direct/pairing.rs:92-125` (`State`, `Request`),
  `:186-295` (`start`), `:297-345` (`reveal`), `:347-360` (`status`), `:384-401` (`deny`), `:406-447` (`approve`);
  persistence on approval in `crates/daemon-server/src/lib.rs:143-167` (`direct_approval`).
- **Source of truth**: `PairingBook`, in memory only; `direct-denied.json` (fingerprints of denied keys, at most
  256). On approval: `service.toml` (`add_device`), `direct-devices.json` (`remember_direct`, origin label only,
  never pruned) and `devices.json` (`approve_service`).
- **States**: `Committed` → `AwaitingApproval{nonce, code}` → `Approved{approval, signature}` | `Denied`; `Expired`
  derived (open and `now ≥ created+120 s`); `stale` (deleted on the next `start`) 10 min after being decided or 12
  min without a decision.

| from | event [guard] | to | effect |
| --- | --- | --- | --- |
| (none) | `start` [valid envelope, not denied, no other request with that device_id] | Committed | new request |
| Committed | `start` repeated identically | Committed | same request returned |
| Committed | `reveal` [open, correct commitment] | AwaitingApproval | code shown |
| AwaitingApproval | `approve` [open, persistence OK] | Approved | persists to `service.toml`, `direct-devices.json`, `devices.json`; `reload_devices` |
| open | `deny` | Denied | fingerprint written to disk |

- **Rejected**: `PairingError::{Invalid, Busy, Denied, Conflict, NotFound, Expired, Internal}`.
- **Terminals**: Approved and Denied (only `stale` deletes them). Nothing reopens them.
- **Persistence and restart**: the whole book is lost; a client that was polling receives `NotFound`.
- **Concurrency**: one `std::sync::Mutex` for the book; `approve` holds it during persistence (service-config flock,
  approval lease, file writes, `reload_devices`). The async handlers `start`, `status`, `reveal` and
  `pending_devices` take the same mutex on the tokio thread (`direct.rs:226-261`, `domain_bootstrap.rs:256-267`),
  so they can block a worker while an approval runs. This is an observation, not an invalid state.

### M-SERVER-D4 · Local terminal pairing
- **Kind**: implicit, two processes. **Defined in**: daemon `auth.rs:270-299` (codes in `Auth.pairing`, TTL 600 s),
  routes `domain_bootstrap.rs:621-673`; client `crates/client-transport/src/local.rs:85-196`.
- **Source of truth**: codes in daemon memory; on the client `tui-device` plus the key file, serialized across
  processes with `tui-pairing.lock` (flock; the file stays but the lock dies with the process).
- **States (client)**: `Unpaired` → (lock) code → `/pair` → deletes `tui-device` → writes the key → writes
  `tui-device` → `Paired`. A cut in the middle leaves «no device» and the next start pairs again
  (`local.rs:157-180`); the daemon keeps an orphan device «Terminal (…)» that expires by disuse.
- **Recovery**: only «wrong signature» triggers a new pairing (`local.rs:273-280`); «device not paired» does not (on
  purpose, test `an_unpaired_device_is_not_paired_again`, `local.rs:797`).

### M-SERVER-D5 · Relay enrollment
- **Kind**: explicit, SQLite. **Defined in**: `service.rs:2425-2430` (`EnrollmentDecision`), `:2638-2715`
  (`reserve/activate/revoke_enrollment`), `:2906-2915` (`enrollment_active`); loop in `service/relay.rs:228-268`
  (`enroll`), `:386-420` (`sync_revocations`), `:443-470` (`keep_registered`).
- **Source of truth**: table `service_enrollments(device_id, signing_key, state)` in `service-receipts.sqlite`;
  relay-side copies are covered by the relay part.
- **States**: `none` → `pending` → `active` | `revoked`. `revoked` is terminal for that id (a new id is needed).

| from | event [guard] | to | effect |
| --- | --- | --- | --- |
| none | reserve [no row] | pending | before any network write |
| pending | POST OK and still approved | active | |
| pending | POST OK but revoked meanwhile | revoked | DELETE on the relay |
| any row not (`active` and approved) | `sync_revocations` | revoked | DELETE on the relay |

  A `pending` row is never retried.
- **Effects of revoked**: deletes push bindings, Live Activity keys and preferences (`service.rs:2688-2703`).
- **Concurrency**: approval lease plus IMMEDIATE transaction.

### M-SERVER-D6 · Composite revocation
- **Kind**: implicit. **Defined in**: `domain_bootstrap.rs:102-109` (`revoke_device`), `:792-812` (route DELETE
  `/devices/{id}`, local only); `device_expiry.rs:54-80` (sweep by disuse / the relay forgot the registration);
  `service/relay.rs:361-384` (account removals).
- **Steps**: the daemon records the revocation locally (M-SERVER-D1, M-SERVER-D2), reloads its devices, and the
  relay loop propagates it to the relay (`sync_revocations`). The keys stay in `service.toml` on purpose.

### M-SERVER-D7 · iroh fingerprints
- **Kind**: implicit. **Defined in**: `iroh_terminals.rs:20-89`; `service.rs:957-1027`; `service/relay.rs:300-350`.
- **Source of truth**: `iroh-terminals.json` (device_id → node + hello signature); bindings already made on the
  relay are in memory (`relay.devices_bound`). Pruned on read (`hellos(keep)`) to the still-approved devices. Every
  iroh stream goes through `handle_direct`, which checks the approval again.

### M-SERVER-D8 · Push recipients
- **Kind**: implicit.
- **Encrypted (service)**: `service.rs:1797-1850`: for each device in `service.toml`, approved in memory plus
  enrollment `active` plus binding. Live Activity: `activity_push.rs:149-160` replaces the phone list on every
  round.
- **Legacy (direct APNs)**: `daemon-support/src/push.rs:48-118`, used when there is no `service.toml`
  (`domain_delivery.rs:244-248`, `push_alerts.rs:40-42`).

### M-SERVER-D9 · Self-update
- **Kind**: implicit. **Defined in**: `daemon-support/src/updates.rs:17-118`; `alondra_core::update::{Attempt,
  Status}`.
- **Source of truth**: `auto-update.json` (last `Attempt{version, at, ok, detail}`, written with a direct
  `fs::write`, no tmp+rename, `:46-52`); `KNOWN` in memory (last version seen).
- **States**: `Up to date` | `Available` → `Updating (brew)` → `Attempted(version, ok)`. `plan()` (`:71-78`): never
  two attempts for the same version, whether it succeeded or failed.

### M-SERVER-D10 · Relaunch after the binary is replaced
- **Kind**: explicit. **Defined in**: `daemon-support/src/relaunch.rs:113-232`; `alondrad/src/lib.rs:170-182`,
  `:329-347`.
- **States**: `Running(identity)` → binary changed → preflight → `Relaunch` (exits with 75 via `exit::now`, without
  draining) | `Rejected(identity, t)` (retried after 10 min). If the binary changes during preflight, the result is
  discarded (`:147-149`). `startup-failures.json` (host crate) counts failed starts.

### M-SERVER-D11 · Service HTTP challenge verifier
- **Kind**: explicit. **Defined in**: `crates/e2e/src/lib.rs:488-612`; used in `auth.rs:347-369`
  (`/service/approve-device`).
- **State**: random per-process `epoch`, nonces used until `timestamp+skew`, clock high-water mark (a clock that
  goes backwards stops accepting requests until it catches up). The nonce is recorded only with a valid signature.

### M-SERVER-D12 · Pending service pairing (CLI, context only)
- `cli/src/service_pair.rs:390-538`: `service-pairing-pending.json` (≤10 min, `relay/src/pairing.rs:12`); continue
  or abandon (abandoning revokes through DELETE `/devices/{id}` on the daemon). A 410 from the daemon (tombstone)
  makes it discard the pending pairing.

### Findings (devices part)

#### F-SERVER-D1 · A transient failure on the first enrollment permanently revokes a device's relay access while it still shows as paired
**Class**: CONFIRMED · **Severity**: S2 · **Probability**: medium-low (only on the first POST of each device, right after pairing) · **Overlap**: none known.
- **Invalid state/transition and broken invariant**: `pending` → `revoked` although nobody revoked the device. It
  ends up «approved in `devices.json` + enrollment revoked»: it gets no pushes and the relay rejects its requests,
  but `/devices` lists it as paired (`domain_bootstrap.rs:675-700` does not show the enrollment state).
- **Evidence**:
  1. `service/relay.rs:236-240` reserves `pending` before the POST.
  2. `:250-252` a `?` on `send`/`error_for_status` exits without going through activate or revoke.
  3. `:463-466` invalidates the epoch.
  4. On the next round, `:394-401` `sync_revocations` revokes every row that is not `active` and sends DELETE.
  5. `service.rs:2652-2656` a `revoked` row returns `Revoked` and `enroll` skips it (`relay.rs:247`).
  6. No code puts a row back to `pending`.
- **Minimal sequence**:
  1. Approve phone `phone-x` (invitation or direct pairing); `reload_devices` → `resync`.
  2. `keep_registered`: bootstrap OK → `enroll` reserves `pending`.
  3. POST `/v1/devices` fails (connection cut, 5xx, 429, the relay restarts, or the daemon relaunches to update
     between the reservation and the response).
  4. Next round: `sync_revocations` → `revoked` + DELETE.
  5. `phone-x` gets no pushes and cannot use the relay; the direct/iroh path keeps working.
- **Impact**: the user believes the phone is paired; outside the LAN it does not work and there is no warning. It
  is fixed only by pairing again with another id. The test at `service.rs:~5700-5795` fixes this behavior for the
  ambiguous case («no implicit re-enrollment after ambiguous outcome»), but the code treats a safe failure
  (connection refused before anything was sent) the same as an ambiguous one.
- **Deterministic test and ablation**: a `Service` test with the in-process fake relay (as at `service.rs:5720`)
  answering 503 to the first POST and 204 afterwards; call `keep_registered` twice and check
  `enrollment_active("phone-x")`. It fails today (the row stays `revoked`). Ablation: remove the
  `pending`→`revoked` part of the fix.
- **Expected fix and files**: tell «not sent / definitively rejected» (go back to `none` or retry) from «possibly
  applied» (compensating DELETE, then retry with the same id, since the signed DELETE leaves the relay in a known
  state), and show the enrollment state in `/devices`. Files: `service/relay.rs`, `service.rs` (Journal),
  `domain_bootstrap.rs::devices`.

#### F-SERVER-D4 · If the Mac's own terminal is revoked, the TUI stays on «device not paired» forever
**Class**: CONFIRMED · **Severity**: S2 · **Probability**: low · **Overlap**: none known.
- **Invalid state/transition and broken invariant**: `tui-device` points to a revoked device; every signed request
  gets 401 and there is no way out from the interface.
- **Evidence**:
  1. `auth.rs:419` / `:479-486` (401 «device not paired»).
  2. `client-transport/src/local.rs:112-123` uses the stored signer without checking it.
  3. `:273-280` pairs again only on «wrong signature».
  4. The route `DELETE /devices/{id}` does not protect the own terminal (`domain_bootstrap.rs:792-812`, unlike
     `device_expiry.rs:45`).
  5. No CLI command deletes `tui-device` (`rg tui-device`).
- **Minimal sequence**: 1) in the Mac app, revoke «Terminal (…)». 2) any TUI screen or reply → 401. 3) same after
  restarting the TUI, until `tui-device` is deleted by hand.
- **Impact**: client unusable; recovered by deleting a file.
- **Deterministic test and ablation**: `an_unpaired_device_is_not_paired_again` (`local.rs:797`) already exists and
  fixes the current behavior; depending on the decision, a `domain_bootstrap` test that refuses to revoke the
  `tui-device`, or a `client-transport` test that offers to pair again.
- **Expected fix and files**: depends on the product decision.

#### F-SERVER-D5 · A direct request from a revoked or conflicting id reaches the user, approval fails with 500 and the client ends up seeing «Expired»
**Class**: CONFIRMED · **Severity**: S3 · **Probability**: low (the «device_revoked» notice asks for a new id) · **Overlap**: none known.
- **Invalid state/transition and broken invariant**: an `AwaitingApproval` that cannot be approved. The user
  compares the code, approves, gets `pairing_failed`, and the client sees `AwaitingApproval` → `Expired` instead of
  «revoked» or «conflict».
- **Evidence**:
  1. `direct/pairing.rs:236-256` `start` checks only the denied list and the in-memory book (neither tombstones nor
     `service.toml`).
  2. `:422-430` a persistence error becomes `Internal` without changing the state.
  3. `lib.rs:150-166` `add_device` fails with «device ID already has other keys» or `approve_service` with `Revoked`.
  4. `direct.rs:209-212` answers 500.
- **Minimal sequence**: 1) client `client-a` was revoked (tombstone) and asks to pair again with the same id.
  2) start + reveal OK; the Mac shows it. 3) the user approves → 500. 4) after 120 s the state is `Expired`.
- **Impact**: confusing outcome for the user.
- **Deterministic test and ablation**: unit test in `pairing.rs`: `approve` with a `persist` that returns `Err`;
  today `status` stays `AwaitingApproval`; expected is a terminal state with a reason, or `start` rejecting the id.
- **Expected fix and files**: check tombstone and key conflict in `start` (passing a query to `PairingBook`) and
  answer `Denied`/`Conflict` before bothering the user. Files: `direct/pairing.rs`, `direct.rs`, `lib.rs`.

#### F-SERVER-D7 · A failed `brew upgrade` blocks self-update of that version until another one is released
**Class**: CONFIRMED · **Severity**: S3 · **Probability**: medium (a `brew upgrade` without network or with the Homebrew lock busy is common) · **Overlap**: none known.
- **Invalid state/transition and broken invariant**: `Attempted(v, ok=false)` is terminal for `v`.
- **Evidence**:
  1. `updates.rs:73` `if last.is_some_and(|a| a.version == latest) { return Plan::Nothing; }` without looking at `ok`.
  2. `:105-109` the attempt is saved even if it failed (no network, 20-minute timeout, `brew` locked).
- **Impact**: the Mac stays on the previous version until the next one; `status` shows it.
- **Deterministic test and ablation**: unit test `plan(true, Some(NEWER), Some(&Attempt{version: NEWER, ok: false,
  ..}), Some(prefix))` → today `Nothing`.
- **Expected fix and files**: retry failures with growing wait and a per-version cap; write `auto-update.json` with
  tmp+rename. File: `updates.rs`.

### Hypotheses (devices part)

#### H-SERVER-D2 · launchd kills `brew upgrade` when the daemon relaunches during the update
`brew` is launched in the same process group (`updates.rs:136-140`, no `process_group`), the plist does not set
`AbandonProcessGroup` (`launchd/alondra.plist.in`), and the watcher (`relaunch.rs:184-212`, every 60 s) exits with
75 as soon as the new link passes preflight, possibly before `brew` finishes (cleanup or links of other binaries)
and before `record_attempt`. Missing: confirm that launchd really kills the group when the job exits, and at which
`brew` step the link identity changes. Probable impact: `auto-update.json` not written (harmless: the new version is
already the latest) or an old keg not cleaned.

### Part 2: relay (`crates/relay`)

### M-SERVER-R1 · Pairing session per service (rendezvous mailbox)
- **Kind**: implicit (`candidate: Option` + `approval: Option` + `expires_at_ms`). **Defined in**:
  `crates/relay/src/pairing.rs:105-112` (struct `Session`), `Mailbox` 114-301.
- **Source of truth**: in-memory `HashMap` behind a `Mutex`. No persistence: a relay restart loses all sessions in
  progress (the Mac and the phone get `404 pairing_not_found`). No copies.
- **States**: `Open` (no candidate) → `Candidate` → `Approved` → `Expired` (implicit: `expires_at_ms <= now`,
  deleted lazily on the next `open`, `pairing.rs:136`).
- **Events**: `POST /v1/pairings` (Mac, `lib.rs:183-195`) → `open`; `POST …/candidates` (phone, with account token
  and proof of its key, `lib.rs:210-226`) → `candidate`; `POST …/approval` (Mac; verifies the approval signature
  with its registered key and that the phone is already enrolled through `/v1/devices`, `lib.rs:228-249`) →
  `approve`; `GET …/approval` (phone, `lib.rs:251-267`); `GET /v1/pairings/{id}` (Mac, `lib.rs:197-208`).

| from | event [guard] | to |
| --- | --- | --- |
| Open | candidate [correct secret, same owner, envelope bound to the session] | Candidate |
| Candidate | candidate (same candidate: idempotent; different: `candidate_exists`) | Candidate |
| Candidate | approval [approval bound to candidate, owner, Mac and expiry] | Approved |
| Approved | approval (same proof: idempotent; other: `approval_exists`) | Approved |

- **Rejected**: typed `&'static str` errors → 404/409/400.
- **Terminals**: `Expired` and purge. Nothing reopens them.
- **Invariants**: one candidate and one approval per session (per code).
- **Purge**: `purge_device`/`purge_owner` (`pairing.rs:115-118`) run only from operator removals
  (`operator.rs:176-187`) and the accounts worker (`management/worker.rs:22-30, 45-48`).

### M-SERVER-R2 · Device registry (phones, Macs, closed accounts)
- **Kind**: implicit composite. **Defined in**: `DeviceRegistry` `auth.rs:86-106`; persistence `persist_registry_at`
  `auth.rs:1836-1881` (atomic write + directory fsync).
- **Source of truth**: the registry JSON (single file) plus an in-memory copy behind a `Mutex`. **Copies that can
  diverge**: (a) revocation tombstones in PostgreSQL (`management_device_revocations`, M-SERVER-R12), immediate
  authority through `require_live_device`, and the JSON, which the worker cleans afterwards; (b) account `status` in
  PostgreSQL versus `closed_owners` in the JSON (filled only by the operator; the `relay_revoke` worker deletes
  devices but does not close the account in the JSON, it relies on PostgreSQL); (c) `live-activity.json`
  (M-SERVER-R6) holds phone→Mac pairings that are pruned against this registry.
- **States per phone id**: `Unknown` → `Enrolled(owner, mac)` → `Revoked` (`revoked_ids`). An unknown id with client
  format can be pre-revoked (`auth.rs:559-561`).
- **States per Mac id**: `Unknown` → `Enrolled` (the pilot Mac's `agreement_key` is completed once,
  `auth.rs:836-848`) → `Revoked` (`revoked_mac_ids`). Per account: `Open` → `Closed` (`closed_owners`, only
  `Removal::Owner`).
- **Events**: `enroll` (`auth.rs:506-540`, write gate), `revoke` (543-593, write gate), `enroll_mac` (820-857,
  atomic through the `Mutex`), `revoke_mac` (976-1007, write gate), `apply_removals`/`reconcile_removals`
  (893-960), `reconcile_device_revocation` (870-876), `revoke_owner` (878-885).
- **Rejected**: typed errors (`EnrollmentError::Conflict`, `AccountGateError`, `RemovalConflict`,
  `RegistryUnavailable`).
- **Invariants enforced at load** (`auth.rs:336-449`): no id both active and revoked, no id both Mac and phone,
  every phone has its Mac, push only for active devices, unique iroh bindings. At run time they hold because every
  mutation checks the same under the `Mutex`.
- **Persistence and restart**: everything survives. A write failure poisons the registry (`registry_poisoned`,
  `auth.rs:1202-1220`): every route answers 503 until restart, because the in-memory rollback is not reliable if the
  failure came after the `rename`.
- **Concurrency**: `device_gate: RwLock` in `Server`; revocations and bindings take write; push/activity sends take
  read and keep it until APNs accepts (`lib.rs:682-683`), so a finished revocation is never followed by a send. No
  read re-entrance (checked: `retry_owed`→`push`→`flush` does not take the gate again; the scheduled task takes it in
  another task).

### M-SERVER-R3 · Per-phone push registration
- **Kind**: implicit: `push[device]` + `push_sessions[device]` + `push_ended ∋ device`. `auth.rs:89-92`.
- **States**: `None` (never registered) / `Registered(token, binding, session?)` / `Ended` (the relay ended it on
  purpose, so the Mac receives `410` and can forget the binding, `lib.rs:593-599`).
- **Events**: `register_push` (supersedes registrations of the same token with the same Mac and owner,
  `auth.rs:596-651`); `unregister_push` (with the binding, 654-675); `forget_ended_session_push` (sweep of offline
  Keycloak sessions every 15 min and operator, 710-728, compares `sid`); `forget_dead_push` (APNs says token dead;
  compares the token, 758-772); revocations (delete push and insert `push_ended`).
- **Invariant**: a revoked device keeps no push (every revocation route deletes it in the same write).

### M-SERVER-R4 · Account removal sequence from the account source
- **Kind**: explicit (`RemovalsApplied::{Through,Gap,Behind}`), `auth.rs:162-170, 893-960`; `removals_pending:
  AtomicBool` (until the first reconciliation, every route that looks at the account answers unavailable,
  `auth.rs:968-974, 1436-1445`).
- **Source of truth**: `removals_seq` in the registry JSON; the external source sends the full list.
- **Transitions**: a batch with `seq ≤ held` is ignored (idempotent); a gap → 409 and a notice to the follower to
  read the full list (`operator.rs:155-163`); a full list adopts its number even if lower. A removal for another
  account is skipped and logged. Nothing is ever restored. Correct as read.

### M-SERVER-R5 · iroh endpoint binding
- **Kind**: implicit (`iroh_nodes[id]` + `proven_iroh_nodes ∋ id`), `auth.rs:97-99, 1010-1134`.
- **States**: `Unbound` / `BoundOnMacWord` / `BoundProven`. A proof signed by the endpoint key replaces a binding
  made on the Mac's word; nothing removes a proven binding. `iroh_node_holder` returns a holder only if the Mac is
  still enrolled; then `require_live_route` applies (tombstones and account).

### M-SERVER-R6 · Live Activity per phone
- **Kind**: composite. **Defined in**: `Phone` `activity.rs:146-172`, `State` 174-185, plan `Phone::plan` 266-279,
  `take` 549-576, `settle` 580-620.
- **Source of truth**: `live-activity.json` plus memory behind a `Mutex`, separate from the registry. `scheduled`
  (in memory) avoids two grouped sends.
- **States of `live`**: `None` / `Some(no update_token)` (started by the relay or by iOS with the app force-quit;
  cannot be ended, and blocks a new `Start` until rotation) / `Some(with token)`. Product with `reports` (active =
  any report with `active`), `pairings`, `held`, `ended`, `unsent`, `urgent_pending`, `last_*`.
- **Events**: `register`/`unregister` (phone), `record` (Mac), `take` (records the send before APNs answers),
  `settle` (APNs response), `prune` (after revocations).
- **Persistence**: everything; a write failure rolls back the in-memory change (`activity.rs:409-420`). There is no
  pruning at startup (see F-SERVER-R4).

### M-SERVER-R7 · Web login flow (OIDC)
- **Kind**: `web_login_flows` rows (PostgreSQL), `web_session/store.rs:161-199`.
- **States**: `Created` (10 min) → `Consumed` (atomic `DELETE … RETURNING` with the browser cookie **and** `state`,
  190-199) / `Expired` (deleted by `create_flow` and `reconcile`).
- **Invariants**: single consumption (two concurrent callbacks: only one gets the row); the ID token `nonce` is
  validated in the exchange (`oidc.rs:186`).

### M-SERVER-R8 · Web session
- **Kind**: explicit in column `refresh_state ∈ {ready, refreshing, revoked}` (CHECK in
  `migrations/0002_web_sessions.sql:19`) + `revision` + row deletion. `store.rs:201-358`,
  `web_session/mod.rs:159-282`.
- **Transitions** (all CAS on `revision` and state, under the session `pg_advisory_lock` of the cookie hash):
  `ready` →begin_refresh→ `refreshing` (before spending the refresh token) →retain_rotated→ `refreshing` (rev+1,
  new secrets) or `revoked` (inactive/expired account) →activate_rotated→ `ready` or `revoked`. `refreshing`
  observed outside the lock → `revoked` (`mod.rs:173-176`). Any state → deleted by logout (`forget_session`) or by
  `reconcile` (`mod.rs:584-602`).
- **Terminals**: `revoked` and deletion; nothing reopens them (`load_session` excludes `revoked`; the CAS require
  `ready`/`refreshing`).
- **Concurrency**: session lock (2 authentication permits), connection with `close_on_drop` so a lock is not
  returned to the pool. See F-SERVER-R5 (hypothesis about cancellation).

### M-SERVER-R9 · Invitation
- **Kind**: explicit `status ∈ {pending, provisioning, accepted, canceled}` (CHECK `0001_management.sql:21`),
  partial unique index on live email (28-29). `management/mod.rs:159-177, 482-516, 600-750, 753-822, 825-840`.
- **Transitions**: `pending` →reserve_acceptance→ `provisioning` (+ account `provisioning`) →complete→ `accepted`
  (+ account `active`); `pending|provisioning` →cancel/reissue/expiry→ `canceled` (+ account `deleting` and outbox).
  `accepted` rejects cancel/reissue (`Conflict`).
- **Concurrency**: acceptance takes a per-invitation session lock (`pg_try_advisory_lock`, fails fast) held during
  the Keycloak provisioning; cancel/reissue take the same transaction lock and wait. Expiry in
  `next_reconciliation` does not take the lock, but `complete` revalidates state, validity and account with `FOR
  UPDATE` and moves to cleanup if something changed (`mod.rs:637-650`). Correct.
- **Composite state observed (S3, not a finding)**: an admin disables a `provisioning` account whose acceptance was
  abandoned → invitation `provisioning` + account `disabled` until the invitation expires (≤7 days); that email
  cannot be re-invited until then without canceling by hand.

### M-SERVER-R10 · Account
- **Kind**: explicit `status ∈ {provisioning, active, disabled, deleting, deleted}` (CHECK
  `0001_management.sql:4`). `change_account_access` `mod.rs:536-597`, `reconciliation_complete` 863-871.
- **Transitions**: `provisioning→active` (only `complete`); `active|provisioning→disabled|deleting`;
  `disabled→deleting`; `deleting→deleted` when all its outbox is completed. There is no reactivation path
  (`disabled→active`): terminal in practice. The last-admin invariant is protected by `ADMIN_LOCK` + `FOR UPDATE`.

### M-SERVER-R11 · Account reconciliation outbox
- **Kind**: `management_outbox(state ∈ {pending, completed}, available_at, attempts)` rows, unique `(account_id,
  action)`. `mod.rs:207-211, 825-871`, worker `management/worker.rs`.
- **Semantics**: claim with a 60 s lease (`FOR UPDATE SKIP LOCKED` + `available_at`), idempotent external
  operations; `keycloak_delete` waits until no other action is pending; completed rows of inactive accounts are set
  back to `pending` every 5 min **forever** (desired tombstones, including `deleted`): periodic work of 3×N deleted
  accounts. A design decision documented in the code.

### M-SERVER-R12 · Device revocation tombstones (account)
- **Kind**: `management_device_revocations(state ∈ {pending, completed})` rows, PK `(kind, device_id)`.
  `management/devices.rs:17-122`.
- **Semantics**: the tombstone is authority once confirmed (`require_live_device` consults it on every authenticated
  route); the worker applies the cleanup to the JSON under the write gate (`worker.rs:21-31`), purges pairings and
  activity, and marks `completed`; it is reviewed again every 5 min. Idempotent on reapply.

### M-SERVER-R13 · Waitlist
- Row present/absent (`management_waitlist`, unique email). `invite_from_waitlist` deletes the row and issues the
  invitation in the same admin transaction (`waitlist.rs:297-321`): a conflict leaves both as they were. Correct.

### M-SERVER-R14 · Admin request idempotency
- `management_admin_requests(actor_id, key_hash)` with the request hash (`mod.rs:213-274`). Reserved inside the
  operation's transaction: if the operation fails, the reservation is rolled back. Retry with the same key:
  `AlreadyApplied`, or `TokenAlreadyIssued` for those that issue a token (the token is not stored: it is lost).

### M-SERVER-R15 · JWKS cache
- `jwt.rs:49-206`: fresh up to `JWKS_MAX_AGE`; an unknown `kid` forces a limited reload; reload every 5 min
  (`lib.rs:1029-1037`). No invalid states observed.

### Findings (relay part)

#### F-SERVER-R4 · If Live Activity pruning fails after a revocation, the removed Mac keeps the Live Activity alive indefinitely
**Class**: CONFIRMED (deterministic path on a write failure or a crash between two writes) · **Severity**: S2 (visible, recoverable with another revocation) · **Probability**: low (disk failure or crash in a short window) · **Overlap**: none known.
- **Invalid state/transition and broken invariant**: `live-activity.json` keeps pairings and reports of a device
  the registry already revoked. Breaks «a revoked Mac does not appear in the Live Activity».
- **Evidence**:
  1. Revocations persist the registry first and then call `activity::after_revocation` (`lib.rs:335-336, 418-419`;
     `operator.rs:186`; `management/worker.rs:30, 48`).
  2. `after_revocation` (`activity.rs:702-713`) logs the `prune` error and returns, with no retry;
     `Store::change` rolls back in memory (`activity.rs:409-420`).
  3. Nothing prunes again: not at startup (`lib.rs:923-1050` does not call `prune`), not `retry_owed`
     (`activity.rs:665-673`, send only). The next prune comes with the next revocation of any account.
  4. Meanwhile the old report with `active=true` keeps `Phone::active()` true (`activity.rs:225-227`), so every
     report from another Mac produces `Update` and, after 7 h 45, `End`+`Start` with an alert (`activity.rs:266-279`).
- **Minimal sequence**: 1) phone with Macs A (report `active=true`) and B; 2) A is revoked; the registry is written;
  3) the write of `live-activity.json` fails (disk full) or the process dies before it; 4) B keeps reporting every
  15 min → the Live Activity keeps showing A and restarts with an alert about every 8 h.
- **Impact**: persistent wrong visible state, start alerts for a removed Mac.
- **Deterministic test and ablation**: unit test on `activity::Store` with a temporary directory: register the
  phone with A and B, save an active report of A, simulate the revocation with `pairing_is_live` returning false for
  A and a failing `prune` (read-only directory), then check that a fresh start (`Store::open` + the startup prune
  the fix adds) stops counting A. Today there is no startup prune and A stays.
- **Expected fix and files**: prune at startup and on every `retry_owed` round (cheap), or prune before writing the
  registry. Files: `crates/relay/src/activity.rs`, `crates/relay/src/lib.rs`.

#### F-SERVER-R6 · A crash between `take` and the send leaves `live = Some(no token)` and blocks the start for up to 7 h 45
**Class**: CONFIRMED (needs a process crash; deterministic path) · **Severity**: S3 · **Probability**: low · **Overlap**: none known.
- **Invalid state/transition and broken invariant**: the relay believes there is a Live Activity it started when
  none was ever sent.
- **Evidence**: `take` persists the plan as sent before calling APNs (`activity.rs:549-576`; `apply` 283-297 sets
  `live = Some(Live{update_token: None})`); `settle` (580-620) is the only thing that undoes it after a failed
  `Start`. After a crash, `plan` cannot end or update without a token and starts again only in the rotation branch
  (`activity.rs:273`), that is 7 h 45 later, unless the phone registers again (`register` overwrites `live`,
  455-459).
- **Impact**: no Live Activity for hours after an unlucky restart.
- **Deterministic test and ablation**: unit test on `Store`: `take` with a planned `Start`, no `settle`, reopen the
  `Store` from the file and check that the next active report produces `Start`. Today it does not.
- **Expected fix and files**: store the `Start` as pending confirmation (with its instant) and treat it as not
  started if there is no `settle` in a window; or do not persist `live` until `settle`. File:
  `crates/relay/src/activity.rs`.

### Hypotheses (relay part)

#### F-SERVER-R5 · Canceling a browser request in the middle of the refresh closes the web session
**Class**: HYPOTHESIS · **Severity**: S3 · **Probability**: medium if cancellation is confirmed.
- **Claim and path**: `authenticate` runs inside the request future (middleware `web_session/mod.rs:315-385`, no
  `tokio::spawn`). Between `begin_refresh` (`mod.rs:185-193`, state `refreshing`) and `activate_rotated`
  (`238-245`), dropping the future releases the lock (the connection closes, `store.rs:83-85`) and leaves
  `refreshing`; the next request sees it outside the lock and revokes (`mod.rs:173-176`). If the drop happens after
  `retain_rotated`, a rotation that did succeed is also thrown away. The code does not protect this window.
- **Impact if confirmed**: the user has to sign in again; it happens in the refresh window (every access-token
  expiry) if the SPA aborts a request (navigation aborts `fetch`).
- **Missing to confirm**: that with the production proxy in front, a browser disconnect closes the connection to
  the relay and hyper/axum then drops the handler future.
- **Proposed test**: a test with disposable PostgreSQL and a fake OIDC provider (pattern of
  `http_logout_works_without_identity_provider…`, `mod.rs:676+`) whose token endpoint blocks on a barrier; abort
  the request task after `begin_refresh`, release the barrier and check that the next request is still
  authenticated. Today it gets 401.
- **Expected fix**: run the `begin_refresh…activate_rotated` span in a separate task (`tokio::spawn`) that holds
  the lock until done, so canceling the request does not interrupt it.

## Clients: terminal, transport, MCP and Apple apps (`CLIENTS`)

Scope: audited at commit `1567967b9`, read-only, nothing executed. Rust part: `crates/client-transport`, `crates/client-port`, `crates/view-model` (`action_id`, `action_outcome`), `crates/cli-tui`, `crates/cli` (`account`, `mac connect`, `device`, `service_pair`), `crates/cli-mcp`. Apple part: `apple/Data/*` (Sync, Transport, Pairing, Iroh, Feed, Notifications, LiveActivity, Account, Attachments), `apple/Features/Sources`, `apple/Contract`, `apple/iOS`, `apple/macOS`, `apple/iOSLiveActivity`, `apple/iOSNotificationService`. Ids `M-CLIENTS-R…`/`F-CLIENTS-R…`/`H-CLIENTS-R…` are the Rust part; `M-CLIENTS-A…`/`F-CLIENTS-A…`/`H-CLIENTS-A…` are the Apple part. Not audited in depth: `crates/cli/src/service_setup/recovery.rs` (replacement of the Mac identity, a durable multi-phase operation); it belongs more to the Mac than to the client.

### Part 1 · Rust clients (terminal UI, transport, CLI, MCP)

Note on dead code: the TUI process never calls `select_relay` (only the delivery command with `--mac` does, `crates/cli/src/delivery.rs:189`), so in the TUI `transport().relay()` and `relay_target()` are always `None`. The relay branches of `Client::for_tui` (`crates/client-port/src/client.rs:152-158`), of the local tab name (`crates/cli-tui/src/tui.rs:1398-1409`) and of `draw.rs:324` are dead code in the TUI. If revived, the "local" tab would mix views of the remote Mac with the pairing notices and daemon identity of the local daemon (`start_pairing_feed`/`start_identity_feed` always use the local daemon, `tui.rs:1346-1388`). Not reachable today.

### M-CLIENTS-R1 · TUI optimistic answers (`Optimistic`)
- **Kind**: explicit (struct with two maps). **Defined in**: `crates/cli-tui/src/tui/optimistic.rs:24-171`; used in `crates/cli-tui/src/tui.rs:1605-1654` (send), `2356-2442` (answer), `2444-2483` (timer), `2075` (reconcile).
- **Source of truth**: TUI process memory, one per Mac tab (`App.optimistic`, `tui.rs:759`). Not persisted: `Restore` (`restore.rs:17-35`) does not include it. **Copies and derivatives**: the daemon keeps a receipt per `command_id` (UUID v4, `client.rs:783-789`).
- **States** per command: `Sending{failures, retry_at: None}` (attempt in flight or just created), `Sending{retry_at: Some}` (waiting for retry), `Sending{overdue: true}` (30 s without confirmation; the card comes back), then removed. Per card: `Applied{version, epoch}` (hidden until a view with another version arrives).
- **Events, guards, effects**:

| From | Event [guard] | To / effect |
|---|---|---|
| none | `begin` (submit) [no equal action in flight] | Sending; card hidden; POST /actions |
| Sending | equal submit [`same_action`: same action_id, option_id, input] | same command, `again`: began=now, overdue=false; the same request is resent |
| Sending | response Unsent/Unanswered/Running | `failed`: retry_at = now + 1,2,4,8,15,15... s |
| Sending (retry_at <= now) | tick | `due`: the same `command_id` is resent |
| Sending | 30 s without confirmation | overdue=true; the card returns with a notice; keeps retrying without limit |
| Sending | response Done/Started | removed; Applied{version = current home} |
| Sending | response Refused/Failed/Unknown/Changed | removed; the card returns if it was not already back |
| Sending/Applied | view of another `data_epoch` | removed without notice |
| Applied | view with a version different from the stored one | removed (the view wins) |

- **Rejected**: a response for an already removed command makes `settle` return None and an empty `Settled` is used (`tui.rs:2374-2375`): only the text is shown.
- **Terminals**: removed. There is no retry cap (by design: "Alondra keeps sending it").
- **Invariants**: resending reuses the `command_id` (enforced in `due`/`same_action`). Assumed: nothing ends the process with live `Sending` entries (broken by F-CLIENTS-R1).
- **Persistence and restart**: everything is lost when the TUI exits or restarts.
- **Concurrency**: only the main thread mutates; POSTs run in threads (`spawn`, `tui.rs:1799-1811`) and come back through a channel. Two POSTs of the same command can be in flight at once (equal submit during an attempt in flight); the daemon deduplicates by `command_id`.

### M-CLIENTS-R2 · Per-tab connection state of the TUI
- **Kind**: implicit.
- **Fields**: `connected`, `offline`, `loaded` (`tui.rs:737-739`), `view_status`, `home_view`, `screen_view_asked`, and, outside `App`, the live transport route `DirectMac::route_kind()` (`direct_client.rs:288-291`, `323-336`).
- **Effective states**: Connecting (`!connected && !offline`), Connected (`connected && !offline`), Offline (`offline`). `connected && offline` is never built because both are always written together (`tui.rs:2086-2087`, `2100-2101`, `2191-2192`, `2196-2197`, `2211-2212`).
- **Writers**: `ViewHome(Ok/Err)` (from `load_screen_view` and from the feed thread), `ViewFeed(Ok/Err)`, `Msg::Offline` (no sender: `background_sync_error` has no callers, `tui.rs:915-921`). Last write wins; there is no ordering between responses.
- **Derivatives read by the UI**: tab (`MacStatus`, `tui.rs:258-293`, `1398-1424`: uses `offline`, `connected` and `route_kind`), red header band (`draw.rs:350-363`: `offline` and `view_status`), waiting screen (`draw.rs:618-633`: `offline`, `loaded`), terminal title (`tui.rs:969`). Three sources: the two booleans and the transport route, which can be `None` with `connected=true` (see F-CLIENTS-R2).
- **Persistence and restart**: none; there is no home cache on disk (overlap #387).

### M-CLIENTS-R3 · Availability probe (`AvailabilityProbe` / `ResponseObserver`)
- **Kind**: explicit. **Defined in**: `crates/client-port/src/client/availability.rs:20-139`. State `{responded, finished}` per request and one shared slot per destination.
- **Transitions**: `begin_request` sets responded=false; `received` (headers arrive before the deadline) sets responded=true; `finish` when the thread ends. `check` reuses the active probe if it has not expired and does not launch a second thread.
- **Invariants**: a late response (after the deadline) does not count (`received` checks the deadline). Any HTTP response counts as "responds", including a 401/421/503 or the response of another Mac on the same IP.

### M-CLIENTS-R4 · Automatic TUI restart on a new daemon
- **Kind**: explicit. **Defined in**: `version_watch::decide` (`crates/cli-tui/src/tui/version_watch.rs:79-104`) over `Watch` (`updated.rs:16-32`); effect in `main_loop` (`tui.rs:1027-1031`) and `restart` (`tui.rs:1043-1062`).
- **Decisions**: Nothing, CheckDisk, Warn(...), Countdown, Restart. Restart when the daemon is newer, the binary on disk is that version, no restart was already done for it, and (requested with R) or (idle >= 10 s and the 3 s countdown elapsed), provided it is not `busy` or `confirming`.
- `busy` = `actions_out > 0` or an attachment uploading (`updated.rs:78-84`). `actions_out` counts only requests in flight: +1 in `action_sent`, -1 when the response arrives (`tui.rs:2066-2068`, `2202-2203`).
- **Persistence and restart**: `Restore` in `$TMPDIR` (screen, selection, composer); nothing else.

### M-CLIENTS-R5 · Pairing notices in the TUI
- **Kind**: implicit. **Defined in**: `pairing: Vec<PendingDevice>` plus `pairing_answered: HashSet` (`tui.rs:797`, `1426-1452`, `2230-2261`). Local feed every `PAIRING_POLL`, sends only on change.
- **Transitions**: answered, removed and marked; error, reinserted and unmarked; the feed removes the marks of devices no longer pending. Correct.

### M-CLIENTS-R6 · Local terminal signature (pairing on first use)
- **Kind**: explicit/composite. **Defined in**: `Pairing{signer cache, replaced}` plus files `tui-device`/`tui-key.pem` plus `tui-pairing.lock` (`crates/client-transport/src/local.rs:85-191`, `send` at `273-284`).
- **States**: no pair, pairing (POST /pairing + /pair, device-last write), pair saved, pair cached. Repair: only if the daemon answers 401 `"wrong signature"` (once per process, `replace_rejected`).
- **Invariants**: enforced: the device file is deleted before the key is written and written last, so a cut never leaves the key of one pair with the device of another.
- **Rejected**: untreated: 401 `"device not paired"` (see F-CLIENTS-R4).

### M-CLIENTS-R7 · Direct connection to a Mac (`Connection`)
- **Kind**: composite. **Defined in**: `direct_client.rs:252-600`; backoff in `iroh_client.rs:257-273`. Current HTTP route (`current: Mutex<Option<String>>`), iroh link `IrohLink{Untried, Open, Down}` plus `Backoff` plus `RelayWatch`, `learned`. **Persistence**: `direct-macs.json` (routes, keys) and the iroh "dial" per Mac, under `direct-macs.lock`.
- **Main transitions**: Untried to Open (connects) or Down (+backoff); Open to Untried (send failure) or Down (`wrong_mac`, 60 s retention); Open stuck on relay to a new connection from a new endpoint. HTTP: a known route is used without `hello`; a new route is validated with `hello` (OtherMac is skipped, KeyChanged aborts).
- **Idempotency**: the same sealed envelope (same `request_id`) goes over iroh and, if lost midway, over HTTP; the Mac executes a `request_id` once. Each new call seals a new `request_id`.
- **Rejected**: HTTP response classification: anything other than 200 with an openable envelope counts as "sent without answer" (see F-CLIENTS-R2).

### M-CLIENTS-R8 · Direct pairing with a Mac (`direct_client::Pairing`)
- **Kind**: explicit by steps. **Defined in**: `direct_client.rs:645-790`, and `connect` (`824-865`).
- **Steps**: start (POST /direct/pairing), reveal, wait (poll 1 s, up to 130 s), then approved (verifies the signature against `hello`) | denied | expired. `connect` saves with `remember` only after verifying.
- **Half-done case**: any transient error in the poll (`.call()?`, `direct_client.rs:760`) aborts and discards the keys; see H-CLIENTS-R1.

### M-CLIENTS-R9 · Terminal relay profile and relay epoch
- **Defined in**: `service_client.rs:29-201`; invitation pairing in `client_pair.rs:110-232` (candidate, approval poll, verification of the signed proof).
- **State**: `Profiles` in `terminal-service.json` under `terminal-service.lock`; process target in `ACTIVE: OnceLock` (immutable per process); `EpochCache{generation, value}`.
- **Retries**: only GET after a 401 with a proven new epoch; POST is never repeated (`service_client.rs:127-147`).
- Used only by the Mac-connect command and by the delivery command with `--mac`.

### M-CLIENTS-R10 · Account session in the CLI
- **Kind**: explicit. **Defined in**: `SessionState{Active, PendingRevocation}` in the keychain plus `AccountLock` (per-user global flock) plus `SERVICE_TOKEN_CACHE` (`crates/cli/src/account.rs:69-83`, `464-535`, `596-656`, `896-1012`).
- **Transitions**: login (revokes and deletes the previous one, then saves Active); refresh (persist the rotated refresh token before `userinfo`, `508-535`); logout: Active to PendingRevocation (persisted) to revocation to deletion; resumable.
- **Rejected**: only `invalid_grant` (400/401) is treated as an expired session (`493-506`); network failures keep the session.
- Keycloak with `revokeRefreshToken=false` (`apple/Data/Account/Sources/Auth/OIDCConfiguration.swift:37-41`): the predecessor stays valid, so two concurrent refreshes do not drop the session. No findings.

### M-CLIENTS-R11 · Device pairing from the Mac (device invite command)
- **Kind**: composite and durable. **Defined in**: `service-pairing-pending.json` plus the device in `service.toml` plus local approval plus registration in the relay plus proof handover (`crates/cli/src/service_pair.rs:390-541`, `625-787`).
- **Resumable**: if a non-expired pending exists, `complete_pending` repeats the idempotent steps; if it expired, `abandon_pending` revokes in the relay (tombstone first), removes it from config, deletes local state and deletes the pending. A 410 from local approval (device removed) abandons. No findings; the pending is only resumed when the command is run again.

### M-CLIENTS-R12 · Delivery of MCP calls from nodes and the orchestrator
- **Kind**: explicit per call. **Defined in**: `crates/cli-mcp/src/mcp.rs:973-1104`. `submit` (waits while the daemon is older, up to 1 h, resending the same command) to `send_command` (cards: up to 3 sends 1 s apart; others: 1) to `send_command_once` (reads `data_epoch` from /capabilities and sends).
- **`command_id`**: cards with `interaction_id` use `mcp-{tool}-{interaction_id}` (`1149-1160`); others use `mcp-native-sha256(operation, tool)`, with operation = hash of native `threadId/sessionId/callId` or of `{server instance, JSON-RPC id}` (`1137-1147`, `1873-1883`).
- **Receipts** at the daemon per `(actor, command_id, data_epoch)` with a payload digest: same payload gives `replayed`; different gives `idempotency_conflict` (`crates/store-sqlite/src/command.rs:111-120`). A refusal leaves no receipt, so resending corrected with the same `interaction_id` works.
- **Quoted images** are copied by SHA-256 (`crates/client-port/src/doc_images.rs:63-95`, `122-135`), so an identical resend produces the same payload. No confirmed findings; see H-CLIENTS-R2/R3.

### M-CLIENTS-R13 · Delivery failure classification (shared)
- `Delivery{Unsent, Unanswered, Running, Unknown, Answered}` and `CommandFailure{Rejected, Uncertain}` (`crates/client-port/src/client.rs:698-766`), plus `transport.delivery` (`client-transport/src/lib.rs:98-101`, `local.rs:316-328`). The TUI, the CLI and the MCP use it to decide whether to retry.

### Findings (Rust part)

#### F-CLIENTS-R1 · The TUI automatic restart throws away answers not yet confirmed
**Class**: CONFIRMED · **Severity**: S2 · **Probability**: medium. The path is correlated: the TUI only restarts by itself when the daemon has updated, which is exactly when answers fail; releases are frequent · **Overlap**: none in open issues; `git log` of `updated.rs`/`optimistic.rs` shows no later fixes.
- **Invalid state/transition and broken invariant**: an optimistic answer is retried until the daemon confirms or rejects it, and if it does not arrive in 30 s the card returns with a notice. The restart deletes it without notice.
- **Evidence**:
  1. When an attempt fails (Unsent/Unanswered/Running) only the next one is scheduled: `tui.rs:2360-2362`, `optimistic.rs:120-125` (waits of 1, 2, 4, 8 and then 15 s).
  2. Between attempts there is no request in flight: `actions_out` rises in `post_action` (`tui.rs:1644`, `updated.rs:221-223`) and falls when the response arrives (`tui.rs:2202-2203`, `updated.rs:225-227`).
  3. `busy()` only looks at `actions_out` and attachments (`updated.rs:78-84`); it does not look at `optimistic`.
  4. `decide` returns Restart with a newer daemon, the same binary on disk, idle >= 10 s and the 3 s countdown (`version_watch.rs:88-101`, `IDLE_MS`/`COUNTDOWN_MS` at `version_watch.rs:6-7`), or when R is pressed (`:94`).
  5. `main_loop` restarts if no tab `holds_restart` (`tui.rs:1027`), which is `busy || confirming` (`updated.rs:91-93`).
  6. `Restore` does not carry `optimistic` (`restore.rs:17-35`); the new process starts with `Optimistic::default()` (`tui.rs:1257`).
- **Minimal sequence**:
  1. A release ships; the daemon restarts to update (stops answering).
  2. The user answers a card in the TUI at t0: it is hidden and the stamp is shown; the POST fails (Unsent) and is rescheduled at +1 s, then +2, +4, +8 (15 s accumulated) and +15.
  3. The daemon returns at t0+20 s. The feed thread (retries every 1 s) receives the view and `Msg::Service` with the new version; the disk check confirms the version.
  4. Idle since t0 (>10 s), then Countdown, then Restart at about t0+24 s. The next attempt of the answer was due at t0+30 s: `actions_out == 0`, `busy == false`.
  5. `exec` of the new binary: the answer has not been delivered and nobody will resend it.
  - Interleaving: the version timer (`on_timers` to `version_tick`, `tui.rs:2571`) wins over `tick_answers` of the same command because the retry wait (8-15 s) is longer than the idle plus countdown window after the daemon returns. With R pressed, any wait between attempts is enough.
- **Impact**: the user saw the card disappear with its stamp and believes they answered. In the new process the card comes back without notice; the task stays at "Necesita tu respuesta" (Needs your answer) until they see it. If the first attempt did go out (Unanswered) it may have been applied and nothing happens; if it was Unsent it is lost.
- **Deterministic test and ablation**: in `crates/cli-tui/src/tui/updated.rs` (module `tests`, `app()` at `:432`): an `App` with `optimistic.begin(request, Some(key), ...)` plus `optimistic.failed(command, now)`, newer `updated.service`, disk with that version, old `last_key_at` and elapsed countdown; assert `version_decision(now) != Decision::Restart` and `holds_restart()`. Today it gives Restart. Ablation: remove the new condition from `busy`.
- **Expected fix and files**: an unconfirmed answer holds the restart (`busy` counts `optimistic` entries in `Sending`), or `Restore` carries the `Sending` entries (request, target, `began`) and the new process keeps resending them with the same `command_id`. Files: `updated.rs`, `restore.rs`, `optimistic.rs`, `tui.rs` (`restore`).

#### F-CLIENTS-R2 · Direct HTTP route: a refusal from the Mac is read as "cut midway"
**Class**: CONFIRMED (client side) · **Severity**: S3 · **Probability**: low · **Overlap**: none. `git log` shows only #471 (iroh backoff), which does not touch `over_http`.
- **Invalid state/transition and broken invariant**: `Delivery` must distinguish "did not go out" / "went out without answer" / "the Mac answered". Over iroh it does; over HTTP it does not.
- **Evidence**:
  1. The direct server answers in the clear 503 `busy`, 400 `invalid_envelope`/`rejected`, 421 `wrong_mac`, 401 `unauthorized`, 409 `idempotency_conflict` (`crates/daemon-server/src/direct.rs:185-207`); 401 comes from an invalid signature or expired envelope (`service.rs:1373-1375`, `crates/e2e/src/lib.rs:212-214`) and from an unknown sender with no revoked record (`service.rs:1416-1419`).
  2. Over iroh those cases are translated: `unauthorized` to "ya no acepta este terminal" (no longer accepts this terminal), `wrong_mac` to NotSent, conflict to 409, the rest to `Answered` (`direct_client.rs:403-445`).
  3. Over HTTP: any status other than 200, or an envelope that does not open, does `sent = true; current = None; continue` (`direct_client.rs:563-583`) and finally `Unanswered` "dejó de responder a mitad de la petición" (stopped answering midway through the request) (`:585-589`). `observer.received()` was already called (`:571`), so the probe says "responds".
- **Minimal sequence** (busy):
  1. The Mac has exhausted its `/direct/request` permits.
  2. The TUI sends an answer over LAN (no iroh open).
  3. 503 gives Unanswered, then `optimistic.failed` and a retry (correct by chance); the CLI says "check before repeating" although nothing ran.
  - Sequence (persistent 401, for example a terminal clock behind by more than the envelope deadline): each request gets 401, then Unanswered. The TUI: `connected = true`, `offline = false` (probe got a response), tab `MacStatus::direct(false, true, None)` = "Conectando…" (Connecting...) (`tui.rs:281-292`) with no offline band; the home does not load or stays stale; each optimistic answer is retried forever and after 30 s warns "Alondra sigue enviándola" (Alondra keeps sending it) indefinitely.
- **Impact**: contradictory indicators (tab "Conectando…", header without problem, stale content), messages that blame the network, and answers that are never resolved nor rejected.
- **Deterministic test and ablation**: in `direct_client.rs` tests (or `direct_client_iroh_tests.rs`), a test HTTP server at `/direct/request` answering 401 `{"error":"unauthorized"}` with the route as `current`; assert `client::delivery(&err)` is `Answered(..)` (as over iroh), and that 503/421 give `Unsent`. Today it gives `Unanswered`. Ablation: go back to `sent = true` for every status.
- **Expected fix and files**: map direct-server statuses the same way as the iroh branch: 421 tries the next route without marking `sent`; 503 means not executed (retryable, `Unsent`); 401 means `not_accepted`; 400/409 give `Answered` with the detail. File: `crates/client-transport/src/direct_client.rs` (`over_http`).

#### F-CLIENTS-R3 · The TUI accepts an old home that arrives late and overwrites the new one
**Class**: CONFIRMED · **Severity**: S3 · **Probability**: low · **Overlap**: #385 (design, requires protection against out-of-order responses), #387 (TUI implementation), #388. The evidence (two producers, `show()`, receiver without guard) is new for them. The criterion is that of #386/#387: a late response never overwrites newer data.
- **Invalid state/transition and broken invariant**: a late response overwrites newer data.
- **Evidence**:
  1. Two producers of `Msg::ViewHome` for the same key: `load_screen_view` (`tui.rs:1528-1543`, own thread, 25 s deadline) and the feed thread on seeing the key obsolete (`view_feed.rs:89-99`).
  2. `show()` sets `home_view = None` and `screen_view_asked = None` (`tui.rs:2723-2724`): going Inbox, Tasks, Inbox launches another "home" request even though the previous one is still in flight.
  3. The receiver only filters by key (`tui.rs:2071-2073`); it compares neither version nor request; it replaces `home_view` and calls `optimistic.reconcile(old version)`, which deletes the `Applied` entries whose stored version does not match (`optimistic.rs:166-169`).
  4. Same with errors: a late `ViewHome(Err)` sets `offline` according to the probe even if the feed just answered (`tui.rs:2099-2105`).
- **Minimal sequence**:
  1. In Inbox, "home" request H1 (slow, direct Mac over relay).
  2. The user goes to Tasks and back: request H2.
  3. In between, the user answers a card and the daemon applies it.
  4. H2 (rendered later) arrives first, without the card.
  5. H1 (rendered earlier) arrives later: the already answered card returns and `Applied` is deleted.
  - The feed corrects it on the next turn (the old version is published in `open_views`, the daemon marks it obsolete).
  - Variant: when settling an answer with `home_view == None` (after `show()`), `shown = None` (`tui.rs:2373`) and the first view to arrive, even one older than the action, brings the card back.
- **Impact**: flicker of old data and of an already answered card; if the user acts on it they get "ya no está disponible" (no longer available).
- **Deterministic test and ablation**: in `crates/cli-tui/src/tui/views_tests.rs`: apply `ViewHome(Ok(v2 without card))` and then `ViewHome(Ok(v1 with card))` from an earlier request; assert the card is not listed. It requires tagging each request (generation per key). Today it is listed. Ablation: remove the generation filter.
- **Expected fix and files**: a generation per view key in `App`; discard responses (Ok and Err) of requests older than the last accepted, or do not relaunch the request if one is already in flight for that key. Files: `tui.rs` (`load_screen_view`, `on_msg`), `view_feed.rs`.

#### F-CLIENTS-R4 · A terminal whose local device was removed stays locked out forever
**Class**: CONFIRMED · **Severity**: S2 · **Probability**: low (not a security hole: it closes access too much, it does not open it) · **Overlap**: none.
- **Invalid state/transition and broken invariant**: the local signing pair is repaired by itself only when the daemon rejects it, and only for one rejection reason.
- **Evidence**:
  1. The signed client presents a signature if a pair is saved (`local.rs:112-124`, `225-231`, `237-247`).
  2. With a signature present, the guard does not apply the local bypass (`crates/daemon-support/src/auth.rs:477-481`) and verifies: absent device gives `"device not paired"` (`auth.rs:416-419`, `433`), then 401 (`auth.rs:469`, `488-491`).
  3. The client only repairs on `"wrong signature"` (`local.rs:276-283`). `"device not paired"` is returned as is.
  4. The files `tui-device`/`tui-key.pem` remain; each new process loads them (`local.rs:149-155`).
  5. The automatic sweep excludes the own terminal (`crates/daemon-server/src/device_expiry.rs:38-50`); the only way is removing it by hand with the device remove command (`crates/cli/src/commands.rs:73-76`) or the local route `DELETE /devices/{id}` (`domain_bootstrap.rs:158`, `792-810`), which lists the terminal in the device list.
- **Minimal sequence**:
  1. The device list shows "Terminal (host)".
  2. The user removes it.
  3. The TUI and every `for_user` command get 401 `device not paired` on each request; the TUI marks `connected=true` (there was a response) and never loads the home; restarting does not help.
- **Impact**: TUI and interactive commands unusable with no hint; it is only fixed by deleting `tui-device` by hand.
- **Deterministic test and ablation**: in `local.rs` `signing_tests`, extend the test `Daemon` (`:554-618`) with a mode that answers 401 `{"error":"device not paired"}` to the saved device; assert that after such a response the `Pairing` pairs again once and the request is repeated successfully. Today it does not repair. Ablation: limit the condition to `"wrong signature"`.
- **Expected fix and files**: treat `"device not paired"` like `"wrong signature"` (repair once per process), or at least delete the saved pair and warn. File: `crates/client-transport/src/local.rs`.

### Hypotheses (Rust part)

#### H-CLIENTS-R1 · Orphan approved device after an aborted terminal pairing
In direct pairing, a transient error in the poll (`direct_client.rs:758-761`, `.call()?`) aborts and discards the keys; in relay pairing the same happens with `relay_call(...)?` (`client_pair.rs:203-209`). If the operator approves on the Mac at the same time or afterwards, the Mac stores a device whose keys no longer exist; repeating the Mac-connect command creates another "Terminal (host)". Missing: checking in the daemon that approving registers the device at that moment (not when the terminal collects the approval). S3; it cleans itself with the sweep of unused devices. Fix: retry the poll on network errors until the deadline.

#### H-CLIENTS-R2 · MCP mutation without a card, repeated after a cut, gives a double effect
The `command_id` of non-card calls depends on the call (`mcp.rs:1137-1147`, `1873-1883`): repeating the tool after "no se pudo contactar con Alondra" (could not contact Alondra) is another command. The text asks to inspect first (`mcp.rs:1186-1188`). Missing: which types the daemon deduplicates by a semantic key (`create_document`, `revise_document`, `create_task`, `start_task`, `register_pr`...).

#### H-CLIENTS-R3 · Orchestrator `launch`/`send` repeated after an expired deadline
They go through `domain_post` without a `command_id` (`mcp.rs:874-881`); the daemon only deduplicates by operation identity (`domain_bootstrap.rs:1058-1066`), which in Claude is `{instance, JSON-RPC id}`, different on each new call. Missing: checking whether `launch_replacing` prevents a second node for the same task.

#### H-CLIENTS-R4 · Clock skew between terminal and direct Mac
A skew larger than the envelope deadline (10-70 s) rejects everything as `Unauthorized`; over iroh the message says "ya no acepta este terminal; vuelve a emparejar" (no longer accepts this terminal; pair again) (`direct_client.rs:208-216`, `419-423`), and pairing again does not fix it. Missing: confirming how the Mac computes `envelope_now` and whether pairing (120 s) also fails.

### Part 2 · Apple apps (iPhone and Mac)

### What is already right

The Apple app is considerably better hardened than the TUI at the points where the Rust part found breakage:
- `FeedSession` already discards out-of-order view responses (owner and lifetime of each load, epoch generation; `apple/Data/Feed/Sources/FeedSession.swift:296-395`), so F-CLIENTS-R3 does not reproduce here.
- Each Mac has its own `Connection` (monitor, feed, answer sender, journal), so a late answer from the previous Mac cannot be applied to the new one (M-CLIENTS-A5).
- Account session: two concurrent requests share the same refresh, a network failure does not drop the session, and concurrent rotation cannot close the session (M-CLIENTS-A6, same as M-CLIENTS-R10 in the CLI).
- Direct pairing retries `URLError` until expiry (M-CLIENTS-A7).

### M-CLIENTS-A1 · Single connection state (`ConnectionMachine` / `ConnectionMonitor`)
- **Kind**: explicit. **Defined in**: `apple/Contract/Sources/ConnectionState.swift:4-100` (state and rules), `apple/Data/Feed/Sources/ConnectionMonitor.swift:7-162` (actor and `MonitoredOperations` wrapper that feeds every request), `apple/Data/Account/Sources/Connection/ConnectionClassifier.swift:8-95` (error to `ConnectionProblem`).
- **Source of truth**: one `ConnectionMonitor` per `Connection` (one per paired Mac, `Connection.swift:199-203`), in memory. **Copies and derivatives**: `AppFeature.State.connection` / `MacAppFeature.State.connection` (copy by subscription, `AppFeature.swift:94-96,106-108`), environment `connectionState` (`AppScene.swift:168`, `MacServiceRoot.swift:165`), `ConnectionIndicators` (light, line and cover derived together, `ConnectionIndicators.swift:30-45`), the overlay (`ConnectionOverlay.swift:9-19`), Settings `MacStatus` (`AccountSession.swift:310-334`, reads `monitor.state`), the `live` flag of the Live Activity (`AppScene.swift:209-214`) and the failure report. There is no second connection source in the UI; `NetworkReachability` only enters as an event (`networkLost/networkBack`) or in classification.
- **States**: `connecting`, `connected(route)`, `unreachable(problem)`. Internal counters: `issued` (tickets), `freshFrom` (from which ticket a failure counts), `passingFailures` (consecutive passing failures).
- **Events, guards, effects**:

| From | Event [guard] | To / effect |
|---|---|---|
| any | `answered(route)` (any answer from the Mac, also a 4xx/5xx from the daemon) | `connected(route)`; `freshFrom = issued`, counter to 0 |
| `connected` | `routeChanged(route)` | `connected(route)`; ignored in other states |
| any | `failed(p, ticket)` [`ticket > freshFrom`, `p != deviceStorage`, `p` not passing] | `unreachable(p)` |
| any | `failed(p, ticket)` [passing: `macNotAnswering`, `serverUnreachable`, `accessNotConfirmed`, `unexpected`] | counter +1; at the 3rd, `unreachable(p)` |
| any | `failed` [`ticket <= freshFrom`] | ignored (request started before the last restart) |
| any | `networkLost` | `unreachable(noInternet)`; `freshFrom = issued` |
| not `needsAction` | `networkBack` | `connecting` (if it was not connected); if it came from `unreachable`, the counter stays at 2 |
| `needsAction` (`sessionExpired`, `deviceRevoked`, `removedFromWeb`, `signOutIncomplete`, `localNetworkDenied`) | `networkBack` | ignored |
| any | `retrying` (user retry, network change, return to foreground) | like `networkBack` but without the `needsAction` guard |

- **Rejected**: old failures (ticket) and `deviceStorage` are silently ignored. There is no terminal: `answered` leaves any problem, also `deviceRevoked`/`sessionExpired` (correct: if the Mac answers sealed, the connection lives).
- **Invariants**: "a request that started before the last answer cannot say the Mac left" (ticket, enforced). "Every indicator derives from this state" (assumed by convention: `ConnectionIndicators` and the environment; the default environment value is `connected(.irohRelay)`, `ConnectionIndicators.swift:62`, so a view outside the root looks connected).
- **Persistence and restart**: none; each start begins in `connecting`.
- **Concurrency**: actor; `begin()` and `handle` in order of arrival at the actor. `MonitoredOperations.observe` takes the ticket before the request (`ConnectionMonitor.swift:127`).

### M-CLIENTS-A2 · Answer delivery (`ViewActionSender` and intent store)
- **Kind**: composite. **Defined in**: memory: `pending: [commandId: Pending]`, `deliveries: [nonce: Task]`, `activeEpoch`, `journalEpoch/journalDescriptor`, withdrawal (`ViewTransport.swift:444-861`). Disk: `ViewActionIntentStore` / `ActionIntentDatabase` encrypted per pairing (`Connection.swift:57-59,110-114`; `apple/Data/Feed/Sources/ActionIntentDatabase.swift`, `ViewActionIntentStore.swift`) with per-record states (created/pending/completed/acknowledged) and quarantine of unreadable stores (`ActionIntentQuarantine.swift`). Plus the sealed-envelope journal per Mac (`EnvelopeJournal`, `Connection.swift:54-56`).
- **Copies and derivatives**: the daemon keeps a receipt per `command_id` and a journal of actions numbered by generation (`ActionOperationID`). Derived: `UndeliveredCards` (cards with an unconfirmed answer in the active epoch, `UndeliveredCards.swift:6-40`, `ViewTransport.swift:779-782`), which reads the queue to hide them after a relaunch.
- **States** per answer: prepared (numbered with the Mac journal generation, `prepare`, `:478-504`), then delivering (`keepDelivering`, loop with a 1...60 s pause or wake by `retryNow`, `:637-662`), then applied (`applied`: complete on disk and leaves `pending`, `:806-810`) | discarded (final error or epoch change, `discard`, `:773-777`) | re-issued with a new id if the seal expired and the Mac says it never finished it (`recoverOrResend`, `:752-771`).
- **Entries**: `prepare` + `deliver` (screens), `send` (legacy), `sendNow` (notification button: one attempt; if it does not arrive, leaves it in `pending` and raises `AnswerKeptForLater`, `:571-595`), `resumeDeliveries(epoch)` (relaunches those of that epoch, `:615-620`), `reconcilePending(epoch)` (asks the Mac about the pending ones, `:830-861`), `retryNow` (wakes the running deliveries, `:610-612`), `stop`.
- **Guards**: same intent gives the same `command_id` (`reserve`, `sameIntent`); one delivery per nonce (`startDelivery`, `:628-635`); epoch different from the active one gives discard (`:651-654`).
- **Persistence and restart**: intents survive relaunch and return to `pending` in `init` (`:463-468`); they are relaunched only from `resumeDeliveries`, which the feed reconciliation calls (`Connection.swift:222-225`, `FeedSession.swift:666-686`). Idempotency at the daemon by `command_id`/operation number.
- **Concurrency**: actor; deliveries are child `Task`s that return to the actor at each step.

### M-CLIENTS-A3 · Optimistic queue UI (`Optimistic` and `TriageFeature`, and `MacPresentationFeature`)
- **Kind**: explicit (`apple/Features/Sources/Shared/Optimistic.swift:10-96`), used by `apple/Features/Sources/Triage/TriageFeature.swift` (iPhone) and by the Mac presentation.
- **Source of truth**: TCA state in memory per screen. It relies on `UndeliveredCards` (M-CLIENTS-A2) to survive a relaunch.
- **States** per card: `sending[nonce] = {target, epoch}` (hidden), `overdue` (30 s without confirmation: visible again, delivery continues), `applied[id] = {version, epoch}` (hidden until a view of another version arrives), and in the queue `answered` (applied, hidden until a view does not list it or 120 s pass, `TriageFeature.swift:159,402-408`).
- **Main transitions**:

| From | Event | To / effect |
|---|---|---|
| none | `.sending(req)` of the card [`resolvesItsCard`] | `sending`; 30 s timer (`:269-272`) |
| sending | 30 s (`answerOverdue`) | `overdue`; the card returns to the front with a notice (`:293-299`) |
| sending/overdue | `.settled(applied: true)` | `applied` + `answered`; the queue is requested (`:380-393`) |
| sending | `.settled(applied: false)` / `handOffUndelivered` | `hold` (hidden until the next new view) and the queue is requested (`:386-389`) |
| overdue | `.settled(applied: false)` | stays visible where it is |
| applied | view with another version or epoch (`reconcile`) | deleted; the daemon wins (`Optimistic.swift:90-95`) |
| sending | view of another epoch | deleted (`Optimistic.swift:91`) |
| applied | daemon `.ask` response (asks again) | `release`: returns to the front (`:278-281`) |

- **Terminals**: deleted. Assumed invariant: every `sending` entry receives a `settled` or the epoch changes (broken by F-CLIENTS-A5).
- **Persistence and restart**: no; after a relaunch the hiding comes from `undelivered` (from disk).

### M-CLIENTS-A4 · State of an on-screen action (`ActionFeature`)
- **Kind**: implicit (flags): `pending`, `inFlight`, `stamp`, `stalled`, `unsure`, `error`, `changed`, `confirmation`, `pendingAttachmentSubmit` (`apple/Features/Sources/ViewsUI/ActionFeature.swift:14-53`).
- **Effective states**: idle; confirming; waiting for attachments; preparing (`inFlight && stamp == nil`); sent (`inFlight && stamp != nil`); stalled (`stalled`, at 10 s, `ViewsClient.swift:69`); answered (success with stamp, `changed` or `error`); doubtful (`unsure`: the same command is reused if repeated identically, `:438-452`).
- **Guards**: `present` does not overwrite a preparation in progress (`:190`); `prepared`/`answered`/`undelivered` only apply if the epoch and `commandId` match `pending` (`:288-330`); changing epoch clears everything (`didSet`, `:45-53`).
- **Effects**: `prepare` then `deliver` with `.cancellable(DeliveryID)`; the real delivery continues in the sender even if the screen goes away (`deliver` launches an unstructured `Task`, `:423-436`).

### M-CLIENTS-A5 · Paired Macs and selected Mac (`AccountSession` and `ApprovedSessionRegistry`)
- **Kind**: composite. **Defined in**: `apple/Data/Account/Sources/Composition/AccountSession.swift` (`generation`, `selection`, `services`, `macs`, `choiceIDs`, `restoreTask`, `attempt`), `apple/Data/Pairing/Sources/ApprovedSessionRegistry.swift` (one live `Connection` per approved pairing and the selected one), pairings in the keychain (`PairingStore.swift`, `PairingCollection.swift`).
- **Source of truth**: keychain (collection and selection) plus the in-memory registry. **Copies and derivatives**: `services` published to the UI (`publishChoices`, `:596-610`); the scene is rebuilt by `Connection` identity (`AppScene.swift:31-32`, `.id(ObjectIdentifier(services))`).
- **Mac switch** (`selectMac`, `:570-628`): one selection at a time (`selection`); it checks that the stored keys match, does a signed round trip to the target Mac (`checkSettingsConnection`), persists the preference and calls `registry.select`; if something fails after persisting, it restores the live selection. `generation` invalidates everything started before a `login`/`logout`/`cancel`.
- **Isolation between Macs**: each `Connection` has its own monitor, feed, sender and intents scoped by `server/mac + device + credential revision` (`Connection.swift:50-59`). The TCA store and the `ViewsClient` are created per scene, bound to that `Connection` (`AppScene.swift:122-138`), so a late answer from the previous Mac only reaches the old store, which is discarded. No finding on this point.
- All connections stay alive in the background (`goLive` opens the iroh connection of all, `:369-372`); only the selected one has a feed (there is no other `viewsFeed` subscription).

### M-CLIENTS-A6 · Account session and token refresh (`AuthSession`)
- **Kind**: explicit. **Defined in**: `apple/Data/Account/Sources/Auth/AuthSession.swift`. State in the keychain: `RefreshCredential{token, status: active | pendingRevocation | rotationUncertain | rejected}`; memory: `accessToken`, `expiresAt`, `refreshTask` (single flight), mutation lock (`reserveMutation`), `epoch`, `transitions`, `sessionSuspended`, `unpersistedCredential`.
- **Refresh** (`refresh`, `:400-449`): marks `rotationUncertain` on disk before sending; `invalid_grant` ends the session (`sessionLost`, `endRejectedSession`); another OAuth error goes back to `active`; a network failure without sending goes back to `active`; a lost response stays `rotationUncertain` and, with `refreshReplay = predecessorStaysValid` (`OIDCConfiguration.swift:41-42`), the next attempt repeats the stored token. One refresh at a time (`refreshTask` plus lock). 401/403 from userinfo or the relay: a refresh is forced at most once a minute and the session is kept (`:240-277`).
- **Classification**: `AuthError.rotationUncertain`/`pendingRevocation`/`noSession` give `sessionExpired` (asks to sign in), `oauth invalid_grant` gives `sessionExpired`; the rest are `accessNotConfirmed`/`serverUnreachable`/`unexpected` (passing) (`ConnectionClassifier.swift:83-94`). With the production configuration, `rotationUncertain` is never thrown (`activeCredential`, `:319-329`).

### M-CLIENTS-A7 · Pairing (QR through relay and direct on the local network)
- **QR through relay**: `PairingClient` (`apple/Data/Pairing/Sources/PairingClient.swift`) with `attempts[UUID]` in memory: candidate sent (202), approval poll (204 wait 2 s, 200 verifies the signed proof and saves in the keychain, 404 expired, other is an error), then saved (replaces the local pairing of the same Mac and selects it). Orchestrated by `AccountSession.begin/approve` with `PairingAttemptLease` (one attempt per scope), `generation` and the expected session. See F-CLIENTS-A3.
- **Direct**: `DirectPairingClient` (`apple/Data/Pairing/Sources/Direct/DirectPairingClient.swift:67-150`): hello, sealed open plus commitment, reveal, poll (retries `URLError` until expiry, unlike QR), verification of the signed approval against the transcript, then save. No findings.
- **Save**: `KeychainApprovedPairingStore` with collection and selection; changes with atomic `modify`.

### M-CLIENTS-A8 · Live Activity
- **Defined in**: `LiveActivityController` (`apple/Data/LiveActivity/Sources/LiveActivityController.swift`).
- **State**: per Mac `{counts, live, heardAt, zeroSince}`; `knowsMacs`, `foreground`, `published`; serial `apply` queue (`tail`). Starts only in foreground with something pending; 15 min grace after reaching zero; expires at 30 min. Source: what the app loaded plus the sealed relay envelopes it reads from the activity itself. Token registration at the relay: `ActivityRegistrar`. See F-CLIENTS-A4.

### M-CLIENTS-A9 · Notifications and push linking
- **Defined in**: `NotificationRuntime.prepare` (`apple/iOS/ViewsShipping/NotificationRuntime.swift:51-112`): for each pairing with relay, one link, one decryption context and one `NotificationClient`; they are published together in a single main-thread commit with `generation`. `deactivate` clears keychain and notifications; pending unregistrations go to a ledger on disk (`PushUnregistrationLedger`). Notification buttons answer through `sendNow` (M-CLIENTS-A2, F-CLIENTS-A1); a normal tap selects the Mac of the push.

### M-CLIENTS-A10 · View loads and feed (`FeedSession`)
- **Defined in**: `apple/Data/Feed/Sources/FeedSession.swift`. Per key, `loadOwners`/`loadLifetimes`/`inFlightLoads` with shared waits; `epochGeneration` invalidates loads of a previous epoch; the feed (`runFeed`) keeps cursor and versions; `reconnect()` restarts capabilities and the poll; `scheduleRecovery` reconciles actions by (generation, epoch). Interactive priority over background (4 at a time, 3 in background).
- **Rejected**: a response from a load that no longer owns the key is discarded or merged; one from another epoch is rejected. There is no on-disk home cache: covered by #386.

### Findings (Apple part)

#### F-CLIENTS-A1 · A notification answer that did not reach the Mac only goes out when that Mac's feed reconciles; with several Macs, the one for a non-selected Mac waits until the user switches to it
**Class**: CONFIRMED · **Severity**: S2 · **Probability**: low (two or more Macs, notification button, Mac unreachable at that moment) · **Overlap**: none in the open issues nor the plans of related work. `git log` of `ViewTransport.swift` shows no changes on this path.
- **Invalid state/transition and broken invariant**: "an answer saved for later goes out by itself as soon as the Mac answers" (contract of `sendNow`, `apple/Data/Feed/Sources/ViewTransport.swift:567-570`; user notice "Your answer will be sent when you open Alondra", `apple/iOS/ViewsShipping/PushNotifications.swift:254-262`) and "something arrived from the Mac: what this device owes it goes out now" (`Connection.macReached`, `apple/Data/Account/Sources/Connection/Connection.swift:183-187`).
- **Evidence**:
  1. A notification button goes to the client of its own pairing (`NotificationRoutingCoordinator.submit`, `apple/Data/Notifications/Sources/NotificationRoutingCoordinator.swift:78-85`), that is, to the `ViewActionSender` of that Mac (`Connection.notificationClient`, `Connection.swift:311-325`, `sendNow`).
  2. `sendNow` makes a single attempt; if it fails with a retryable error it puts the answer in `pending` and on disk and raises `AnswerKeptForLater` without starting any delivery (`ViewTransport.swift:584-594`). The app shows "Couldn't reach your Mac ... will be sent when you open Alondra" (`PushNotifications.swift:238-239,254-262`).
  3. An answer in `pending` is only started by `startDelivery`, which `deliver` (screens) and `resumeDeliveries` call (`ViewTransport.swift:604,615-620`). `retryNow` only wakes deliveries already running (`:610-612`, `pause` at `:668-676`); `macReached` and `networkChanged` only call `retryNow` (`Connection.swift:179,186`).
  4. `resumeDeliveries` is only called by the feed reconciliation of that connection (`Connection.swift:222-225`), which `FeedSession` launches when loading a view or in the feed loop (`scheduleRecovery`, `FeedSession.swift:666-686`, which requires `dataEpoch`, known only after a load).
  5. Only the selected connection has screens and therefore loads and feed: the store and `ViewsClient` are created for `owner.services` (`apple/iOS/ViewsShipping/AppScene.swift:30-32,122-138`); there is no other consumer of `viewsFeed` on iOS except Settings of the selected one (`SettingsScene.swift:43`). The other connections stay alive (`AccountSession.goLive`, `AccountSession.swift:369-372`) but without a feed.
  6. `macReached` is installed with the selected connection (`AppScene.swift:177`), so a push from Mac B wakes the deliveries of Mac A.
- **Minimal sequence**:
  1. iPhone with Macs A (selected) and B paired by account. A notification from B with buttons arrives.
  2. B does not answer at that moment (asleep, network). The user taps "Approve" in the notification: `sendNow` fails, the answer stays in B's `pending`; the notification says it will go out when Alondra is opened.
  3. B comes back and sends another push (or the user opens Alondra): A's feed reconciles and `macReached` wakes A.
  4. B's answer does not go out. It would only go out if the user selects B in Settings (then B's feed starts, reconciles and calls `resumeDeliveries`).
- **Impact**: B's task stays at "Necesita tu respuesta" (Needs your answer) and the user does not know (they think they answered and that it went out on opening the app); B may notify the same card again, and if the user answers something else from elsewhere, switching to B sends the old answer (the daemon rejects it by revision if the card changed).
- **Deterministic test and ablation**: in `apple/Data/Feed/Tests/NotificationAnswerTests.swift`, with the existing `DistantMac`: `sendNow` gives `AnswerKeptForLater`; `mac.comeIntoReach()`; `await sender.retryNow()` (what `macReached` does); `await sender.drainDeliveries()`; assert `mac.runs == [kept.commandId: 1]`. Today `runs` stays empty (there is no delivery to wake). For the multi-Mac part, a `Connection`/`AccountSession` test with two test pairings and a double transport: answer kept in B, scene `macReached`, assert that B delivers it without being selected. Ablation: remove the fix (that `sendNow`/`retryNow` starts pending deliveries).
- **Expected fix and files**: a saved answer starts its own background delivery (`startDelivery` when saving it in `sendNow`, or `retryNow` starts the `pending` ones of the known epoch that have no delivery), and `macReached` is called on the connection of the Mac that sent the push (or on all). Files: `ViewTransport.swift`, `Connection.swift`, `NotificationRuntime.swift`/`AppScene.swift`.

#### F-CLIENTS-A2 · In Tasks and in the task detail (iPhone) the optimistic action has no notice at 30 s: withdrawing a request, deleting Completed or cancelling/pausing a task stays hidden while the Mac does not confirm
**Class**: CONFIRMED · **Severity**: S2 · **Probability**: medium (any such action with the Mac not answering; frequent on iPhone) · **Overlap**: none; `git log` of both files shows no changes to this logic since `Optimistic` was added.
- **Invalid state/transition and broken invariant**: optimistic UI rule "silent retries, and the interaction returns with a notice after 30 s". The queue (`TriageFeature.confirmationTimer`, `TriageFeature.swift:410-418`) and the Mac (`MacPresentationFeature.swift:626-630`) comply; Tasks and the task detail do not.
- **Evidence**:
  1. `TasksFeature` hides what was sent (`board` = `TasksBoard(...).hiding(optimistic)`, `apple/Features/Sources/Tasks/TasksFeature.swift:25-27`; `TasksBoard.hiding`, `TasksBoard.swift:141-154`: the withdrawn request and the delete-Completed button disappear). `TaskDetailView` hides the sent task buttons (interrupt, resume, relaunch, pause, model, cancel, reopen) and shows the chosen priority (`apple/iOS/UI/Sources/Tasks/TaskDetailView.swift:401,423,437`).
  2. They enter `sending` on prepare (`.actions(.sent)` to `begin`, `TasksFeature.swift:129-131`, `TaskDetailFeature.swift:91-93`) and only leave with `.settled` (`TasksFeature.swift:132-134`, `TaskDetailFeature.swift:94-96`). Neither has a timer nor `expire` (only `Optimistic.swift` has `expire`; its only users are the queue and the Mac).
  3. `.settled` arrives when `viewsClient.perform` ends (`ActionFeature.swift:423-436`), which is `ViewActionSender.deliver`: it retries endlessly while the error is retryable and only throws on a final error (`ViewTransport.swift:597-606,637-662`).
  4. Incoming views do not release it: `reconcile(version, dataEpoch)` only filters `sending` by epoch (`Optimistic.swift:90-95`).
  5. On iPhone neither `TasksView` nor `TaskDetailView` shows `StallNoticeView` (only `ActionInputView` and the Mac use it), so the "Couldn't send · Retrying..." at 10 s does not appear either.
- **Minimal sequence**:
  1. iPhone with the home and Tasks loaded; the Mac goes to sleep (or the relay cannot reach it).
  2. In Tasks, the user withdraws a request: the row disappears at once.
  3. Minutes pass: the row stays hidden, with no notice; delivery retries silently.
  4. The user closes the app: on return the request reappears with no explanation (`Optimistic` lived only in memory; the answer is still in `pending` and goes out when the feed reconciles).
  - Same with "Cancel" in a task detail: the button disappears and the task keeps working on the Mac.
- **Impact**: the user believes the task was cancelled or the request withdrawn when the Mac does not even know; in the cancel case the task keeps spending. If the Mac finally rejects it, the row returns with no visible notice except the toast.
- **Deterministic test and ablation**: a TCA reducer test (`TestStore`) of `TasksFeature` and `TaskDetailFeature` with `continuousClock = TestClock` and a `viewsClient.perform` that never returns: send `.perform(withdraw)`, advance the clock `Optimistic.confirmationTimeout`; assert the row returns (`board` lists it) and a notice is emitted (`.delegate(.failed(Optimistic.notConfirmed(...)))` or equivalent). Today there is no action to receive and the row stays hidden. Ablation: remove the new timer.
- **Expected fix and files**: the same pattern as the queue: a confirmation timer per target on `begin`, `expire` on expiry with a notice, and cancel the timer in `settle`. Better still, move the timer to a common reducer that wraps `Optimistic` so no screen forgets it. Files: `TasksFeature.swift`, `TaskDetailFeature.swift` (and review the other screens that call `optimistic.begin`).

#### F-CLIENTS-A3 · QR pairing fails halfway if the access token expires or there is a cut while waiting for the Mac's approval; the Mac can be left with a device without keys
**Class**: CONFIRMED on the phone side; the orphan device on the Mac is also confirmed by reading the CLI · **Severity**: S2 (pairing that fails for no real reason; recovered with a new QR; fails closed: the phone keys are discarded and nobody can use the orphan device) · **Probability**: medium (the token may have only about 30 s of life at the start; the user takes longer than that to type the code on the Mac) · **Overlap**: same pattern as H-CLIENTS-R1 (terminal pairing) in the Rust part; here with a new, deterministic cause (fixed token). No open issue.
- **Invalid state/transition and broken invariant**: "a pairing ends approved and saved on both sides, or on neither"; and the account rule "a network or token failure never asks to start over".
- **Evidence**:
  1. `AccountSession.begin` takes a `bearerToken()` (`apple/Data/Account/Sources/Composition/AccountSession.swift:522`) that may have only a little over 30 s of life (`AuthSession.bearerToken` returns the cached token if more than 30 s remain, `AuthSession.swift:206`), and fixes it in the attempt (`PairingAuthorization`, which `PairingClient.connection` uses as `bearer: { authorization.bearerToken }`, `PairingClient.swift:205-211`).
  2. `waitForApproval` polls every 2 s until the QR expires (up to 10 min, `PairingClient.swift:108,145-156`) with that fixed token. The relay validates the JWT `exp` on each poll (`authenticate_pairing_candidate` to `verify_jwt`, `crates/relay/src/auth.rs:1377-1380`; `c.exp <= now_s` gives an error, `crates/relay/src/jwt.rs:189-195`) and answers 401.
  3. `RelayTransport` returns that 401 as is (it only retries `relay_epoch_changed`, `RelayTransport.swift:211-219`; the default `bearerRefused` does nothing); `waitForApproval` turns it into `unexpectedStatus(401)` (`PairingClient.swift:195-196`). The same for a passing `URLError` or a relay 5xx: they leave the loop with `throw`.
  4. `AccountSession.approve` discards the attempt on any error (`pairing.discard`, `AccountSession.swift:543-549`): the phone's private keys are lost.
  5. On the Mac, approving the candidate adds the device to `service.toml` and registers it in the relay before publishing the approval (`complete_pending`, `crates/cli/src/service_pair.rs:409-479`), so if the owner approves after the phone gave up, a device whose key pair no longer exists remains.
- **Minimal sequence**:
  1. The user scans the QR when the token has 60 s left.
  2. The phone shows the comparison code.
  3. The user goes to the Mac, opens the terminal and types the code at 90 s.
  4. At 60 s the poll already received 401: the phone says it could not pair.
  5. The Mac approves: "iPhone de ..." appears in the device list of the Mac and the relay, but that phone does not exist. The user repeats with a new QR and a second device remains.
- **Impact**: failed pairing for no reason, orphan device visible on the Mac until the sweep of unused devices removes it.
- **Deterministic test and ablation**: a `PairingClient` test with a double `HTTPTransport` and an injected clock (`now`): the relay double answers 204 while `now < exp` of the token and 401 afterwards; advance the clock past `exp` before the approval and then answer 200 with a valid proof. Today `waitForApproval` throws `unexpectedStatus(401)`; the right behavior is to renew the token (ask `AuthSession` for a new one) and continue. Another case: a `URLError(.networkConnectionLost)` in a poll must retry until QR expiry, not abort. Ablation: go back to the fixed `bearer` / the `throw`.
- **Expected fix and files**: the attempt stores a token function (`{ try await auth.bearerToken() }`, with `bearerRefusedForToken` to force renewal after a 401), and passing poll errors wait and retry until `expiresAtMs`. Files: `PairingClient.swift`, `PairingModels.swift` (`PairingAuthorization`, if its shape changes), `AccountSession.swift` (`begin`/`approve`).

#### F-CLIENTS-A4 · After switching Mac, the Live Activity keeps showing the previous Mac's frozen numbers as "just measured" and covers the sealed reports the relay brings from it
**Class**: CONFIRMED · **Severity**: S2 (wrong visible state on the lock screen; it can also prevent the activity from ending, or end it while the other Mac is still busy) · **Probability**: medium for anyone with two Macs (any Mac switch leaves the previous one frozen until the app is killed) · **Overlap**: none (Live Activity is not in the plans of related work nor in open issues). `git log`: last change #213 (categories), not touching this.
- **Invalid state/transition and broken invariant**: "the app only writes over what the relay pushed for the Macs it has newer news about" (`LiveActivityController.swift:35-36`; `docs/architecture/live-activity.md`, "The app writes the Macs it is connected to in the clear").
- **Evidence**:
  1. Only the selected Mac's scene reports: `liveActivity.controller.report(macID: services.macID, ..., live: store.connection.isConnected)` (`apple/iOS/ViewsShipping/AppScene.swift:183-187,209-214`). On Mac switch the scene is rebuilt (`.id(ObjectIdentifier(services))`, `AppScene.swift:31-32`) and nobody sends a last `report(..., live: false)` for the previous Mac; `keep(macIDs:)` only removes unpaired Macs (`LiveActivityController.swift:89-93`, `AppScene.swift:55-57`).
  2. On each `apply`, every Mac with `live == true` gets `heardAt = now` (`LiveActivityController.swift:118`), enters `known` even if there is a sealed report (`:128`), is published with `measuredAt = now` (`:129-131`) and makes the relay's sealed report for that Mac count as old and be removed from the state (`outdated`, `:132-134`).
  3. The extension sums the newest report per Mac by `measuredAtMs` (`ActivityContentState.swift:78-83`), so the frozen one wins.
  4. `busy` and the end of the activity are decided with those numbers (`:135-144`): if the previous Mac had cards at the switch, the activity never ends; if it had zero and later fills, the app ends it on the next `apply` even though the relay says it is busy.
  5. Each relay push replaces the whole content (without `reports`) and corrects the screen, but the next `apply` (any change of the new Mac's counts, going to foreground or background, the ActivityKit notice) writes the frozen numbers again.
- **Minimal sequence**:
  1. Mac A selected with 3 cards, connected: the app reports A=3, `live=true`.
  2. The user switches to Mac B in Settings.
  3. In A the 3 cards are answered from the terminal; the relay sends A's sealed envelope with 0.
  4. Any change of B arrives or the app goes to background: `apply` publishes A=3 with the current time and removes A's envelope; the lock screen says 3 in the tray again and the activity does not close.
- **Impact**: wrong visible state (see severity); the activity cannot end or ends wrongly.
- **Deterministic test and ablation**: in `apple/Data/LiveActivity/Tests/LiveActivityControllerTests.swift` with the existing `Harness`: `report(3, mac: "mac-a", live: true)`; `report(0, mac: "mac-b", live: true)`; `harness.now += 60`; `harness.opened = [ActivityReport(macID: "mac-a", ..., counts: 0, measuredAt: harness.now)]`; `background()`. Assert the last published state gives 0 for mac-a and the activity ends (or is in grace). Today it publishes 3 for mac-a with `measuredAt` later than the envelope. Ablation: remove the fix.
- **Expected fix and files**: when a Mac stops being shown (scene change, background) report `live: false` for it, or let the controller refresh `heardAt` only for the active scene's Mac; the timestamp of an app report must be that of the last loaded view, not of the `apply`. Files: `LiveActivityController.swift`, `AppScene.swift`.

#### F-CLIENTS-A5 · If preparing an answer fails, the queue (iPhone) and the sidebar (Mac) leave the card "sending" forever
**Class**: CONFIRMED · **Severity**: S3 · **Probability**: low (prepare only fails in a final way: unreadable local store, revoked device, Mac journal that disappears) · **Overlap**: none.
- **Invalid state/transition and broken invariant**: every `sending` entry of `Optimistic` ends in `settle` or an epoch change (M-CLIENTS-A3).
- **Evidence**:
  1. `ActionFeature.send` emits `.sending(request)` before preparing and, if `prepare` fails, only `.undelivered(request, ...)`, without `.settled` (`apple/Features/Sources/ViewsUI/ActionFeature.swift:413-420`; `.settled` only comes from `deliver`, `:423-436`).
  2. `TriageFeature` starts the optimism at `.sending` (`TriageFeature.swift:269-272`) and does not handle `.actions(.undelivered)` (it falls into `case .cards, .delegate: return .none`, `:323-324`); the Mac likewise (`MacPresentationFeature.swift:328-335`, `358-361` does not call `settle`).
  3. At 30 s `answerOverdue` shows it again with "Your Mac hasn't confirmed your answer..." (false text: it never went out), and `isSending` stays true: `forgetOverdue` keeps it overdue and `keepWindowObserved` never releases its state (`Optimistic.swift:59-61`, `TriageFeature.swift:445-449`).
  4. If the user answers it again successfully, `settle` sees `isSending` and does not remove it from the queue until the next view (`TriageFeature.swift:383-392`).
- **Minimal sequence**:
  1. The encrypted intent store fails to write (disk full).
  2. The user answers a card: it disappears.
  3. The card shows the real error for an instant (it is hidden).
  4. At 30 s it returns with "your Mac has not confirmed", and stays marked as sending until the epoch changes.
- **Impact**: false notice and a card stuck as sending; a later successful answer is not removed from the queue until the next view.
- **Deterministic test and ablation**: a `TestStore` of `TriageFeature` with `viewsClient.prepare` throwing `ViewActionIntentError` and `TestClock`: after `.submit` on the card, assert the card returns immediately (without waiting 30 s) and `optimistic.isSending(id) == false`. Today it waits 30 s and stays `isSending`. Ablation: remove the new `.settled(request, applied: false)`.
- **Expected fix and files**: emit `.settled(request, applied: false)` also when preparation fails (or treat `.undelivered` as `settle`) in `ActionFeature.send`. File: `ActionFeature.swift`.

### Hypotheses (Apple part)

#### H-CLIENTS-A1 · Answers of a previous epoch stay on disk forever
`resumeDeliveries` and `reconcilePending` only touch those of the current epoch (`ViewTransport.swift:615-620,850`) and `prepare` only removes them from memory (`:496-499`); nothing discards them from the store after a Mac epoch change if there is no running delivery that sees the active epoch differ (`:651-654`). Missing: checking in `ActionIntentDatabase` whether they count toward the capacity cap (`capacityExceeded`); if they do, after several epoch changes the iPhone would be unable to prepare answers.

#### H-CLIENTS-A2 · On removing a Mac, its pending answers stay encrypted on disk
`forgetMac` to `stop` to `Connection.deactivate` does not delete `action-intents` nor `operations` of that scope (`AccountSession.swift:275-283`, `ApprovedSessionRegistry.swift:92-95`). Missing: checking whether something purges them at startup. Only disk space, S3.
