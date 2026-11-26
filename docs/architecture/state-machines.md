# State machines

Every explicit and implicit state machine of Alondra: its states, source of truth, invariants,
allowed and rejected transitions, terminals, persistence across restarts and concurrency. It also
lists the invalid states and transitions the code can reach, through retries, duplicates,
cancellations and concurrency, and the plan that fixes them. This is the single canonical file for
it: whoever changes one of these machines updates its section here in the same pull request.

Each section starts with a `toml state-machine` block: the code that defines the machine (`path::Symbol`, never a
line), where its truth lives, and, when one enum is its state, that enum's variants and terminals. The `state-files`
block in [M-STORE-12](#m-store-12--state-in-json-files-in-the-home-folder-implicit-machines-split-with-sqlite) names
the one writer of every state file, and the `timers` block in
[Timers that act on state](#timers-that-act-on-state-outside-obligation_policy) says of every time constant whether
it decides state or only paces. A block may also declare `initial` and `transitions` (`"From -> To"`): today
M-LIFE-1 (the task's phase), M-GH-2 (the merge attempt) and M-LIFE-9 (the node's phase). Every command the domain's
tests and the machine model run fails if it makes a move its block does not declare. `//tools:state_inventory_test` fails when the code and these blocks disagree (a
section, symbol, variant, state file, table, timer or state enum the other side lacks); what is not state is in
`tools/state_inventory_exempt.tsv` and what an open fix owns in `tools/state_inventory_known.tsv`. The judgment the
test cannot make is the `state-machines-review` check of `MUSTS.yml`, with the questions of
[state-machines-review.md](state-machines-review.md).

- **Audited at**: commit `1567967b9` of `main` (2026-11-22).
- **Re-checked**: PRs #505, #507 and #536 against commit `3fc0370ea` (2026-11-22): M-LIFE-1, M-LIFE-10,
  M-LIFE-14, M-CARDS-10, M-CARDS-13 (new), M-ENGINE-4, M-ENGINE-5, M-ENGINE-10, M-ENGINE-25, M-PROV-8 and the
  timers table updated; F-LIFE-1, F-LIFE-2, F-CARDS-1, F-ENGINE-12 and H-CARDS-5 re-checked (still hold); new
  findings F-STORE-7 (fixed since by G3, #520) and F-CARDS-7. Text this re-check added or refreshed says "at `3fc0370ea`" and its line numbers
  refer to that commit; every other line number still refers to the audited commit.
- **Re-checked**: PRs #516, #537, #540 and #541 against commit `68770a298` (2026-11-22): M-LIFE-5, M-LIFE-14,
  M-ENGINE-5, M-ENGINE-8, M-ENGINE-28 (new), M-GH-5, M-ORCH-1, M-ORCH-5, M-ORCH-6 (removed), M-ORCH-7, M-ORCH-13
  (new), M-PROV-4, M-PROV-9 and the timers table updated; F-ENGINE-1, F-ENGINE-5, F-STORE-3, F-STORE-4, F-GH-5,
  F-CARDS-1, F-ORCH-1, F-ORCH-2, F-ORCH-3, F-ORCH-6, F-PROV-2 and F-PROV-3 re-checked; H-ORCH-3 no longer reachable;
  new hypothesis H-ORCH-4; new evidence for the referred tasks. Text of this pass says "at `68770a298`".
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
| S1 | **FIXED** [F-GH-2](#f-gh-2--an-already-answered-delete-unsaved-changes-card-deletes-the-recreated-worktree-of-a-reopened-task-on-every-closer-pass) · An already-answered "delete unsaved changes?" card deletes the recreated worktree of a reopened task, on every closer pass | low | [G1](#g1--never-delete-what-a-live-task-or-the-user-still-owns) · #518 |
| S1 | **FIXED** [F-GH-4](#f-gh-4--closing-a-task-with-a-merged-pr-deletes-the-branch-of-a-remote-less-repo-another-repo-with-commits-that-exist-only-there-pr-503) · Closing a task with a merged PR deletes the branch of a remote-less repo (another repo) with commits that exist only there (PR #503) | low | [G1](#g1--never-delete-what-a-live-task-or-the-user-still-owns) · #518 |
| S1 | **FIXED** [F-GH-10](#f-gh-10--a-delete-answer-about-a-launch-folder-the-domain-never-recorded-is-applied-again-on-every-closer-pass) · A «delete» answer about a launch folder the domain never recorded is applied again on every closer pass | low | [G1](#g1--never-delete-what-a-live-task-or-the-user-still-owns) · #548 review |
| S1 | **FIXED** [F-GH-11](#f-gh-11--the-startup-sweep-deletes-a-folder-another-finished-task-kept-with-leave-and-commits-not-pushed) · The startup sweep deletes a folder another finished task kept with «Leave», and commits not pushed | low | [G1](#g1--never-delete-what-a-live-task-or-the-user-still-owns) · #548 review |
| S1 | [F-STORE-7](#f-store-7--a-request-cleared-and-then-taken-back-by-its-task-leaves-its-cleared-row-and-the-daemon-no-longer-loads-its-state) · A request cleared and then taken back by its task leaves its «cleared» row, and the daemon no longer loads its state | low | [G3](#g3--the-daemon-survives-its-own-restarts-and-recoverable-errors) · #520 · fixed in `2fc1e9349` |
| S1 | [F-ORCH-5](#f-orch-5--a-non-domain-error-in-the-schedule-runner-stops-the-whole-daemon-with-an-unreadable-optionstoml-it-enters-a-restart-loop) · A non-domain error in the schedule runner stops the whole daemon; with an unreadable `options.toml` it enters a restart loop | low | [G3](#g3--the-daemon-survives-its-own-restarts-and-recoverable-errors) · #520 · fixed in `2fc1e9349` |
| S1 | [F-STORE-4](#f-store-4--a-released-task-whose-closed-task-nodesjson-entry-is-lost-is-left-without-a-node-forever-nothing-relaunches-it-or-says-so) · A released task whose `closed-task-nodes.json` entry is lost is left without a node forever: nothing relaunches it or says so | low | [G2](#g2--a-released-task-always-has-a-way-back) · #519 · fixed in #552 |
| S2 | [F-CARDS-1](#f-cards-1--a-request-whose-tasks-ended-depends-on-a-single-message-to-the-orchestrator-if-that-batch-does-not-close-it-it-stays-open-forever-with-no-result-card-issue-353) · A request whose tasks ended depends on a single message to the orchestrator: if that batch does not close it, it stays open forever with no result card (issue #353) | medium | [G5](#g5--every-request-and-task-has-someone-who-moves-it-next) · #522 · fixed |
| S2 | [F-CARDS-2](#f-cards-2--replacing-one-question-of-a-group-leaves-the-group-incomplete-forever-the-node-never-receives-the-other-answers) · Replacing one question of a group leaves the group incomplete forever: the node never receives the other answers | low | [G6](#g6--card-protocol-replacements-approvals-and-stale-deliveries) · #523 · fixed in #551 |
| S2 | [F-CARDS-3](#f-cards-3--requesting-changes-or-just-replying-on-an-already-approved-plan-while-its-task-closes-disapproves-it-but-leaves-the-implementation-request-alive-the-plan-is-implemented-twice-or-with-the-old-version) · Requesting changes (or just replying) on an already approved plan while its task closes disapproves it but leaves the implementation request alive: the plan is implemented twice or with the old version | low-medium | [G6](#g6--card-protocol-replacements-approvals-and-stale-deliveries) · #523 · fixed in #551 |
| S2 | [F-CARDS-4](#f-cards-4--a-new-revision-of-a-document-leaves-dead-the-final-delivery-that-carries-the-previous-one-neither-accepting-nor-requesting-changes-works-and-nobody-returns-it) · A new revision of a document leaves dead the final delivery that carries the previous one: neither accepting nor requesting changes works and nobody returns it | low | [G6](#g6--card-protocol-replacements-approvals-and-stale-deliveries) · #523 · fixed in #551 |
| S2 | [F-CARDS-5](#f-cards-5--replacing-the-current-final-delivery-with-a-card-that-is-not-another-final-delivery-withdraws-it-without-returning-it-the-task-stays-delivered-with-no-card-for-the-user) · Replacing the current final delivery with a card that is not another final delivery withdraws it without returning it: the task stays "Delivered" with no card for the user | low-medium | [G6](#g6--card-protocol-replacements-approvals-and-stale-deliveries) · #523 · fixed in #551 |
| S2 | **FIXED** [F-CLIENTS-A1](#f-clients-a1--a-notification-answer-that-did-not-reach-the-mac-only-goes-out-when-that-macs-feed-reconciles-with-several-macs-the-one-for-a-non-selected-mac-waits-until-the-user-switches-to-it) · A notification answer that did not reach the Mac only goes out when that Mac's feed reconciles; with several Macs, the one for a non-selected Mac waits until the user switches to it | low | [G10](#g10--apple-apps-optimistic-actions-and-per-mac-state) · #527 · fixed |
| S2 | **FIXED** [F-CLIENTS-A2](#f-clients-a2--in-tasks-and-in-the-task-detail-iphone-the-optimistic-action-has-no-notice-at-30-s-withdrawing-a-request-deleting-completed-or-cancellingpausing-a-task-stays-hidden-while-the-mac-does-not-confirm) · In Tasks and in the task detail (iPhone) the optimistic action has no notice at 30 s: withdrawing a request, deleting Completed or cancelling/pausing a task stays hidden while the Mac does not confirm | medium | [G10](#g10--apple-apps-optimistic-actions-and-per-mac-state) · #527 · fixed |
| S2 | **FIXED** [F-CLIENTS-A3](#f-clients-a3--qr-pairing-fails-halfway-if-the-access-token-expires-or-there-is-a-cut-while-waiting-for-the-macs-approval-the-mac-can-be-left-with-a-device-without-keys) · QR pairing fails halfway if the access token expires or there is a cut while waiting for the Mac's approval; the Mac can be left with a device without keys | medium | [G10](#g10--apple-apps-optimistic-actions-and-per-mac-state) · #527 · fixed |
| S2 | **FIXED** [F-CLIENTS-A4](#f-clients-a4--after-switching-mac-the-live-activity-keeps-showing-the-previous-macs-frozen-numbers-as-just-measured-and-covers-the-sealed-reports-the-relay-brings-from-it) · After switching Mac, the Live Activity keeps showing the previous Mac's frozen numbers as "just measured" and covers the sealed reports the relay brings from it | medium | [G10](#g10--apple-apps-optimistic-actions-and-per-mac-state) · #527 · fixed |
| S2 | **FIXED** [F-CLIENTS-R1](#f-clients-r1--the-tui-automatic-restart-throws-away-answers-not-yet-confirmed) · The TUI automatic restart throws away answers not yet confirmed | medium | [G9](#g9--terminal-client-and-local-pairing-recover-by-themselves) · #526 · fixed |
| S2 | **FIXED** [F-CLIENTS-R4](#f-clients-r4--a-terminal-whose-local-device-was-removed-stays-locked-out-forever) · A terminal whose local device was removed stays locked out forever | low | [G9](#g9--terminal-client-and-local-pairing-recover-by-themselves) · #526 · fixed |
| S2 | [F-ENGINE-1](#f-engine-1--the-slot-of-an-admitted-start-expires-after-3-min-even-if-the-node-does-not-exist-yet-the-gate-admits-above-the-limit) · The slot of an admitted start expires after 3 min even if the node does not exist yet: the gate admits above the limit | low | [G4](#g4--the-admission-gate-counts-what-is-really-running) · #521 · fixed in `e7b898c30` |
| S2 | **FIXED** [F-ENGINE-12](#f-engine-12--a-retake-in-progress-and-the-adoption-of-a-model-transition-take-the-same-host-at-once-a-false-host-lost-card-or-two-live-owners-for-one-agent-formerly-h-engine-1) · A retake in progress and the adoption of a model transition take the same host at once: a false "host lost" card, or two live `Owner`s for one agent | low | [G8](#g8--one-owner-per-host-and-provider-facts-in-order) · #525 · fixed in #613 |
| S2 | [F-ENGINE-3](#f-engine-3--the-pause-of-a-queued-task-lives-in-admissionjson-apart-from-launch_queue-it-can-be-lost-duplicated-or-stay-forever) · The pause of a queued task lives in `admission.json`, apart from `launch_queue`: it can be lost, duplicated or stay forever | — | [G4](#g4--the-admission-gate-counts-what-is-really-running) · #521 · fixed in `e7b898c30` |
| S2 | [F-ENGINE-6](#f-engine-6--the-watch-advances-the-disk-incident-on-every-look-not-on-every-sample-with-watched-volumes-any-domain-change-restarts-the-recovery-and-without-them-one-sample-counts-twice) · The watch advances the disk incident on every look, not on every sample: with watched volumes any domain change restarts the recovery (and without them one sample counts twice) | medium | [G4](#g4--the-admission-gate-counts-what-is-really-running) · #521 · fixed in `e7b898c30` |
| S2 | [F-ENGINE-7](#f-engine-7--the-stretches-of-build-volumes-do-not-survive-a-restart-working-nodes-then-reserve-at-most-1-gb-there) · The stretches of build volumes do not survive a restart: working nodes then reserve at most 1 GB there | medium | [G4](#g4--the-admission-gate-counts-what-is-really-running) · #521 · no longer applies (`e7b898c30`) |
| S2 | [F-ENGINE-9](#f-engine-9--a-volume-that-is-not-a-build-volume-can-stay-learned-forever-and-hold-every-start-and-the-turns) · A volume that is not a build volume can stay learned forever and hold every start and the turns | low | [G4](#g4--the-admission-gate-counts-what-is-really-running) · #521 · no longer applies (`e7b898c30`) |
| S2 | **FIXED** [F-GH-1](#f-gh-1--the-watcher-closes-on-github-the-pr-of-a-task-the-user-just-reopened-or-that-another-task-just-took) · The watcher closes on GitHub the PR of a task the user just reopened (or that another task just took) | low-medium | [G7](#g7--github-no-closing-merging-or-reading-against-a-stale-picture) · #524 |
| S2 | **FIXED** [F-GH-3](#f-gh-3--the-startup-sweep-of-cancelled-tasks-deletes-a-worktree-that-task-handed-to-another-if-the-other-is-already-closing-or-the-user-kept-it) · The startup sweep of cancelled tasks deletes a worktree that task handed to another, if the other is already closing or the user kept it | low | [G1](#g1--never-delete-what-a-live-task-or-the-user-still-owns) · #518 |
| S2 | [F-GH-5](#f-gh-5--post-merge-work-declared-and-the-node-idle-the-task-stays-working-forever-its-host-is-never-released-and-it-holds-a-slot) · Post-merge work declared and the node idle: the task stays "Working" forever, its host is never released and it holds a slot | medium-low | [G5](#g5--every-request-and-task-has-someone-who-moves-it-next) · #522 · fixed |
| S2 | **FIXED** [F-GH-7](#f-gh-7--a-merge-that-github-blocks-for-a-required-review-goes-back-to-the-node-and-invalidates-the-users-acceptance-in-a-loop) · A merge that GitHub blocks for a required review goes back to the node and invalidates the user's acceptance, in a loop | medium | [G7](#g7--github-no-closing-merging-or-reading-against-a-stale-picture) · #524 |
| S2 | **FIXED** [F-GH-8](#f-gh-8--the-github-api-limit-still-raises-technical-cards-one-per-pr-in-the-watcher-and-another-per-pr-in-the-merger) · The GitHub API limit still raises technical cards: one per PR in the watcher and another per PR in the merger | low | [G7](#g7--github-no-closing-merging-or-reading-against-a-stale-picture) · #524 |
| S2 | [F-LIFE-1](#f-life-1--an-unstarted-task-that-waits-for-other-tasks-shows-as-queued-slot-wait-not-waiting-for-tasks) · An unstarted task that waits for other tasks shows as "Queued" (slot wait), not "Waiting for tasks" | high | [G5](#g5--every-request-and-task-has-someone-who-moves-it-next) · #522 · fixed |
| S2 | [F-LIFE-2](#f-life-2--reopening-a-task-whose-sdk-node-the-user-left-open-creates-a-claude---bg-execution-nobody-runs) · Reopening a task whose SDK node the user left open creates a `claude --bg` execution nobody runs | low | [G2](#g2--a-released-task-always-has-a-way-back) · #519 · fixed in #552 |
| S2 | [F-ORCH-1](#f-orch-1--a-daemon-restart-in-the-middle-of-a-batch-stops-the-orchestrator-until-the-user-answers-a-card) · A daemon restart in the middle of a batch stops the orchestrator until the user answers a card | medium | [G3](#g3--the-daemon-survives-its-own-restarts-and-recoverable-errors) · #520 · fixed in `2fc1e9349` |
| S2 | [F-ORCH-2](#f-orch-2--replanning-reminders-feed-themselves-one-batch-per-reminder-without-end) · Replanning reminders feed themselves: one batch per reminder, without end | low-medium | [G5](#g5--every-request-and-task-has-someone-who-moves-it-next) · #522 · fixed |
| S2 | [F-ORCH-3](#f-orch-3--after-24-h-of-launch-failures-requests-are-left-untreated-and-the-watchdog-restarts-in-vain-every-5-min) · After 24 h of launch failures, requests are left untreated and the watchdog restarts in vain every 5 min | low | [G3](#g3--the-daemon-survives-its-own-restarts-and-recoverable-errors) · #520 · fixed in `2fc1e9349` |
| S2 | [F-ORCH-4](#f-orch-4--a-scheduled-launch-that-did-start-is-considered-failed-after-a-restart-and-answering-the-card-cancels-the-live-task) · A scheduled launch that did start is considered failed after a restart, and answering the card cancels the live task | low | [G3](#g3--the-daemon-survives-its-own-restarts-and-recoverable-errors) · #520 · fixed in `2fc1e9349` |
| S2 | **FIXED** [F-PROV-1](#f-prov-1--a-usage-reading-measured-before-a-limit-erases-it-when-it-arrives-later-the-account-reads-as-having-margin-again) · A usage reading measured before a limit erases it when it arrives later: the account reads as having margin again | low | [G8](#g8--one-owner-per-host-and-provider-facts-in-order) · #525 · fixed in #613 |
| S2 | **FIXED** [F-SERVER-D1](#f-server-d1--a-transient-failure-on-the-first-enrollment-permanently-revokes-a-devices-relay-access-while-it-still-shows-as-paired) · A transient failure on the first enrollment permanently revokes a device's relay access while it still shows as paired | medium-low | [G11](#g11--devices-relay-and-self-update) · #528 · fixed |
| S2 | **FIXED** [F-SERVER-D4](#f-server-d4--if-the-macs-own-terminal-is-revoked-the-tui-stays-on-device-not-paired-forever) · If the Mac's own terminal is revoked, the TUI stays on «device not paired» forever | low | [G9](#g9--terminal-client-and-local-pairing-recover-by-themselves) · #526 · fixed |
| S2 | **FIXED** [F-SERVER-R4](#f-server-r4--if-live-activity-pruning-fails-after-a-revocation-the-removed-mac-keeps-the-live-activity-alive-indefinitely) · If Live Activity pruning fails after a revocation, the removed Mac keeps the Live Activity alive indefinitely | low | [G11](#g11--devices-relay-and-self-update) · #528 · fixed |
| S2 | [F-STORE-1](#f-store-1--a-second-process-applies-restart-recovery-to-the-live-database) · A second process applies restart recovery to the live database | low | [G3](#g3--the-daemon-survives-its-own-restarts-and-recoverable-errors) · #520 · fixed in `2fc1e9349` |
| S2 | [F-STORE-2](#f-store-2--alondrad---preflight-does-not-exercise-the-json-files-the-real-startup-requires) · `alondrad --preflight` does not exercise the JSON files the real startup requires | low | [G3](#g3--the-daemon-survives-its-own-restarts-and-recoverable-errors) · #520 · fixed in `2fc1e9349` |
| S3 | [F-CARDS-6](#f-cards-6--createtask-and-setdependencies-accept-an-already-cancelled-dependency-the-task-is-born-waiting-for-something-that-will-not-come-issue-323) · `CreateTask` and `SetDependencies` accept an already cancelled dependency: the task is born waiting for something that will not come (issue #323) | medium | [G5](#g5--every-request-and-task-has-someone-who-moves-it-next) · #522 · fixed |
| S3 | [F-CARDS-7](#f-cards-7--clear-completed-takes-a-closed-request-whose-last-task-is-still-ending-which-the-tasks-list-now-shows-as-active-536) · «Clear completed» takes a closed request whose last task is still ending, which the Tasks list now shows as active (#536) | medium | [G5](#g5--every-request-and-task-has-someone-who-moves-it-next) · #522 |
| S3 | **FIXED** [F-CLIENTS-A5](#f-clients-a5--if-preparing-an-answer-fails-the-queue-iphone-and-the-sidebar-mac-leave-the-card-sending-forever) · If preparing an answer fails, the queue (iPhone) and the sidebar (Mac) leave the card "sending" forever | low | [G10](#g10--apple-apps-optimistic-actions-and-per-mac-state) · #527 · fixed |
| S3 | **FIXED** [F-CLIENTS-R2](#f-clients-r2--direct-http-route-a-refusal-from-the-mac-is-read-as-cut-midway) · Direct HTTP route: a refusal from the Mac is read as "cut midway" | low | [G9](#g9--terminal-client-and-local-pairing-recover-by-themselves) · #526 · fixed |
| S3 | **FIXED** [F-CLIENTS-R3](#f-clients-r3--the-tui-accepts-an-old-home-that-arrives-late-and-overwrites-the-new-one) · The TUI accepts an old home that arrives late and overwrites the new one | low | [G9](#g9--terminal-client-and-local-pairing-recover-by-themselves) · #526 · fixed |
| S3 | [F-ENGINE-10](#f-engine-10--disk_room_for-decides-the-next-one-goes-alone-with-a-disk_working-that-pace-can-empty-mid-call) · `disk_room_for` decides "the next one goes alone" with a `disk_working` that `pace` can empty mid-call | very | [G4](#g4--the-admission-gate-counts-what-is-really-running) · #521 · fixed in `e7b898c30` |
| S3 | **FIXED** [F-ENGINE-11](#f-engine-11--two-states-of-the-hook-transport-are-unreachable-and-their-documentation-describes-behaviour-that-does-not-exist) · Two states of the hook transport are unreachable and their documentation describes behaviour that does not exist | always | [G12](#g12--the-architecture-docs-match-the-code) · #529 · fixed in #624 |
| S3 | [F-ENGINE-2](#f-engine-2--auto_launch-admissions-that-never-launch-hold-a-slot-for-up-to-3-min) · `auto_launch` admissions that never launch hold a slot for up to 3 min | low | [G4](#g4--the-admission-gate-counts-what-is-really-running) · #521 · fixed in `e7b898c30` |
| S3 | [F-ENGINE-4](#f-engine-4--a-turn-with-a-granted-slot-loses-its-place-in-the-queue-after-a-restart) · A turn with a granted slot loses its place in the queue after a restart | medium | [G4](#g4--the-admission-gate-counts-what-is-really-running) · #521 · fixed in `e7b898c30` |
| S3 | **FIXED** [F-ENGINE-5](#f-engine-5--task-statesmd-describes-a-timer-that-no-longer-exists-and-omits-the-ones-that-do) · `task-states.md` describes a timer that no longer exists and omits the ones that do | not | [G12](#g12--the-architecture-docs-match-the-code) · #529 · fixed in #624 (the part not referred) |
| S3 | **FIXED** [F-ENGINE-8](#f-engine-8--own_worktree-does-not-delete-the-worktree-of-a-task-that-ended-while-starting-the-domain-command-rejects-it-before-reaching-that-branch) · `own_worktree` does not delete the worktree of a task that ended while starting: the domain command rejects it before reaching that branch | low | [G1](#g1--never-delete-what-a-live-task-or-the-user-still-owns) · #518 |
| S3 | **FIXED** [F-GH-6](#f-gh-6--an-attachment-of-a-user-message-to-an-open-task-is-deleted-after-30-days-even-though-the-task-is-still-open) · An attachment of a user message to an open task is deleted after 30 days even though the task is still open | low | [G1](#g1--never-delete-what-a-live-task-or-the-user-still-owns) · #518 |
| S3 | **FIXED** [F-GH-9](#f-gh-9--the-merger-rereads-github-on-every-domain-commit-for-a-pr-closed-outside-or-with-a-new-head-until-the-watcher-sees-it) · The merger rereads GitHub on every domain commit for a PR closed outside or with a new head, until the watcher sees it | medium | [G7](#g7--github-no-closing-merging-or-reading-against-a-stale-picture) · #524 |
| S3 | [F-ORCH-6](#f-orch-6--the-orchestrators-progress-marks-have-done-nothing-since-277) · The orchestrator's progress marks have done nothing since #277 | low | [G3](#g3--the-daemon-survives-its-own-restarts-and-recoverable-errors) · #520 · fixed in `2fc1e9349` |
| S3 | [F-ORCH-7](#f-orch-7--an-accepted-cancellation-http-200-can-be-lost-orchestratorjson-is-written-without-a-lock-from-two-threads) · An accepted cancellation (HTTP 200) can be lost: `orchestrator.json` is written without a lock from two threads | very | [G3](#g3--the-daemon-survives-its-own-restarts-and-recoverable-errors) · #520 · fixed in `2fc1e9349` |
| S3 | [F-ORCH-8](#f-orch-8--create_task-carries-no-priority-and-automatic-launch-wakes-in-the-same-commit-issue-352) · `create_task` carries no priority and automatic launch wakes in the same commit (issue #352) | medium | [G5](#g5--every-request-and-task-has-someone-who-moves-it-next) · #522 · fixed |
| S3 | **FIXED** [F-SERVER-D5](#f-server-d5--a-direct-request-from-a-revoked-or-conflicting-id-reaches-the-user-approval-fails-with-500-and-the-client-ends-up-seeing-expired) · A direct request from a revoked or conflicting id reaches the user, approval fails with 500 and the client ends up seeing «Expired» | low | [G11](#g11--devices-relay-and-self-update) · #528 · fixed |
| S3 | **FIXED** [F-SERVER-D7](#f-server-d7--a-failed-brew-upgrade-blocks-self-update-of-that-version-until-another-one-is-released) · A failed `brew upgrade` blocks self-update of that version until another one is released | medium | [G11](#g11--devices-relay-and-self-update) · #528 · fixed |
| S3 | **FIXED** [F-SERVER-R6](#f-server-r6--a-crash-between-take-and-the-send-leaves-live--someno-token-and-blocks-the-start-for-up-to-7-h-45) · A crash between `take` and the send leaves `live = Some(no token)` and blocks the start for up to 7 h 45 | low | [G11](#g11--devices-relay-and-self-update) · #528 · fixed |
| S3 | [F-STORE-3](#f-store-3--retry-of-an-uncertain-orchestrator-message-older-than-24-h-discards-itself) · "Retry" of an uncertain orchestrator message older than 24 h discards itself | medium | [G3](#g3--the-daemon-survives-its-own-restarts-and-recoverable-errors) · #520 · fixed in `2fc1e9349` |

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

**Status**: FIXED (PR #548). Each finding below carries its test and ablation.

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
- **Status: fixed** in #552 (commit `56d796f26`, from `main` at `10ebafced`). Before the fix, `main` at
  `10ebafced` still had all three: the entry lived only in `closed-task-nodes.json`, `holder()` was only called from
  tests, and `ReopenTask` decided `Admitted{execution: None}` for a kept node. Final behaviour:
  - `ReleaseTask` carries `relaunch` (who starts the next node) and the closed node's `cwd`; the rule stores
    `Task.released = ClosedNode{name, cwd, relaunch}` in the same transaction, and the store keeps it in the table
    `task_released_nodes`, written by the same commit. The release `RecordNodeEnd` makes for a task that went back to
    work stores `ClosedNode{…, relaunch: ByItself}` the same way. `materialize` clears it when a node takes the task
    or the task ends or starts ending. `closed-task-nodes.json` is no longer written or read; a daemon that starts
    deletes the old file. Every reader (`auto_launch::pass`, `start_failures::facts`, `relaunch_from`, the replace
    path, the server's Tasks lists) reads `Task.released`.
  - `DomainState::react`, which the daemon runs on every automatic-launch pass (`start_retries::react`), now asks
    `holder()`: an unstarted task launched before, with no node, no open card and no record of its closed node, is
    `Ownerless` and gets the `NodeClosed` card («Relaunch or cancel»). Its «try again» relaunches it
    (`start_retries::retry` relaunches any task launched before, and `relaunch_from` no longer needs the record: it
    takes the task's worktree, then the project). This also covers a task an older daemon released whose file entry
    was lost before the upgrade.
  - `ReopenTask` never keeps the node: every reopened task is left with no node and no execution, goes back to the
    launch queue with its seniority, and the gate's SDK start resumes the saved session (`pending_reopening`). The
    domain no longer records a `claude-bg` execution on reopen.
  - Evidence (tests, each ablated by hand; output in #552):
    `domain-rules` `tests::a_released_task_keeps_who_starts_its_next_node_in_the_commit_that_releases_it`,
    `ownership_tests::a_released_task_with_no_record_of_its_closed_node_is_put_to_the_user`,
    `sdk_node_tests::a_task_reopened_with_its_sdk_node_kept_by_the_user_waits_for_a_new_execution_that_is_heard`;
    `engine-sdk` `auto_launch::tests::a_node_alondra_closed_is_relaunched_by_itself_by_the_daemon_that_restarts_after_it`
    (release, the old file cannot be written, restart: relaunched by itself, no card) and
    `auto_launch::tests::a_task_reopened_with_its_node_kept_by_the_user_gets_a_new_execution_that_resumes_its_session`.

### G3 · The daemon survives its own restarts and recoverable errors

Issue: #520 · Severity: S1 (F-STORE-7, F-ORCH-5), S2 otherwise · Findings: F-STORE-7, F-ORCH-5, F-ORCH-1,
F-ORCH-3, F-ORCH-4, F-ORCH-6, F-ORCH-7, F-STORE-1, F-STORE-2 (the part not referred), F-STORE-3 · Depends on:
none · **Fixed in `2fc1e9349`**: every finding was reproduced on `main` (093f26be0) by its new test, failing, before the
fix; the ablation output is in the pull request.

- **Expected behaviour**: a failure of one scheduled run (unreadable options, a launch error, a
  non-ordinary claim error) is recorded on that run and the schedule worker goes on; only storage
  errors stop the daemon, if that policy stays. A batch of the orchestrator cut by a restart resumes
  without the user (default decision). Boot failures of the orchestrator never leave requests
  untreated for 24 h. A scheduled launch that did start is recognised as started after a restart, and
  answering its card never cancels the live task. `orchestrator.json` has one writer. A second process
  never applies restart recovery to the live database. `--preflight` reads every JSON file startup
  requires. A manual retry of an expired orchestrator message is refused with a clear message. The
  orchestrator's progress marks either drive its watchdog again or are removed. Every field of the domain state the
  store keeps in a side table is written both ways: a request that stops being cleared loses its `cleared_requests`
  row in the same commit, so a reload reads what was committed.
- **Files**: `crates/store-sqlite/src/repository.rs` (request side tables), `crates/engine-sdk/src/domain_schedule_runner.rs`, `crates/alondrad/src/lib.rs`,
  `crates/engine-sdk/src/domain_orchestrator.rs` and the orchestrator inbox steps, the store's
  startup recovery, `crates/daemon-support/src/auth.rs` (preflight reads), the delivery CLI.
- **Tests**: `run_cycle` with a pending run and an invalid `options.toml` returns `Ok` and leaves the
  run pending with its reason (today `Err`; ablation: restore the `?` at `domain_schedule_runner.rs:415`);
  the orchestrator restart test through the path real startup takes (today the test only exercises
  `recover()`); a store test opening a second recovery against a live database.
- **Final behaviour** (what the code does since the fix):
  - F-STORE-7: saving a request that is no longer cleared deletes its `cleared_requests` row, so a request a task
    took back (`AdoptOpenPr`, `reopen_requests_with_open_tasks`) loads open after a restart.
  - F-ORCH-5: a scheduled run's failure is that run's: unreadable settings leave it Pending with the launch refusal
    `launch_failed` and the detail «Alondra's settings file can't be read: …» (the reasons `launch_refusals`
    accepts are a closed SQL set; a new one would need a schema migration), its slot goes back to the gate and the
    next cycle retries it; any other non-storage error of one run is logged and the other runs go on.
    Only a storage error (`StoreError::Storage` or SQLite) still stops the schedule worker, and the daemon with it.
  - F-ORCH-4: an uncertain scheduled launch whose task already has a node is recorded Handed to that node; only a
    task without a node fails its run. Answering a «could not start» card never cancels or relaunches a task that
    has a node.
  - F-ORCH-1: on SIGTERM, a new release or a handover the daemon writes `cut_by_shutdown` (the active batch) into
    `orchestrator.json`. The next start resumes that batch without a card: confirmed effects are handed off, a
    batch with no effect is offered again, and unconfirmed operations go to the next batch as evidence in its
    handoff. A batch cut any other way (a crash that also lost its terminal report) keeps asking: it may have lost
    a refusal, which existing tests protect (`a_claude_denial_cannot_be_replayed_…`).
  - F-ORCH-3: when the messages a failed start was retrying have all expired, the failure no longer holds the inbox:
    `last_error`, `failure` and `retry_at` are cleared and `settle` sends the open requests back. The watchdog
    no longer restarts the executor while a start's backoff is waiting.
  - F-ORCH-6: batch claim, host start, `handled`, recovery and each tool operation's admission and end count as
    progress again (`note_orchestrator_progress`); the read `orchestrator_progress` is `#[must_use]`.
  - F-ORCH-7: `orchestrator.json` has one writer at a time (`Orchestrator.record`); the cancel and the host's start
    read, change and write their field under that lock, so neither lands between the other's read and write.
  - F-STORE-1: the daemon holds `daemon.lock` exclusively before it opens the domain database; a second `alondrad`
    on the same HOME is refused before its restart recovery runs. `--test-service-push` never opens the domain
    database: it reads the service's devices and sends (the documented default was «sent by the live daemon through
    its API»; the push needs nothing from the domain, so the diagnostic process simply no longer opens it).
  - F-STORE-2 (the part not referred): the preflight snapshot also carries `domain-capabilities.json`,
    `devices.json`, `options.toml`, `options.pending`, `orchestrator.json` and `admission.json`, with their bytes and
    permission bits, so a file the real startup refuses makes preflight fail. The binary smoke test stays with the
    tests-by-layer work.
  - F-STORE-3: retrying an uncertain orchestrator message older than 24 h is refused with `delivery_expired` («…more
    than 24 hours old… discard it instead»); discarding it still works.
- **Tests** (each failed on `main` before its fix): `store-sqlite` `tests::a_cleared_request_its_ended_task_takes_back_loads_open_after_a_restart`,
  `tests::retrying_an_uncertain_orchestrator_message_past_its_24_hours_is_refused_and_discarding_it_works`;
  `engine-sdk` `domain_schedule_runner::tests::unreadable_settings_leave_a_pending_run_waiting_with_its_reason_and_the_runner_going_on`,
  `…::a_launch_cut_after_its_node_was_admitted_is_handed_to_that_node_and_answering_never_cancels_it`,
  `…::answering_a_failed_launch_card_leaves_a_task_that_got_its_node_running`; `engine-kernel`
  `batch_operations::tests::admitting_and_finishing_a_tool_operation_each_count_as_orchestrator_progress`;
  `engine-orchestrator` `recovery_decisions::a_batch_cut_by_the_daemons_stop_is_offered_again_on_the_next_start_without_a_card`,
  `…::a_batch_cut_by_the_daemons_stop_hands_its_unconfirmed_operation_to_the_next_batch_as_evidence`,
  `…::a_batch_cut_without_the_daemons_stop_still_asks_before_anything_runs_again` (guard, passes before and after),
  `tests::the_hosts_start_holds_the_record_from_its_read_to_its_write_so_a_cancel_cannot_land_in_between`,
  `tests::a_request_whose_message_expired_during_failed_starts_goes_back_to_the_orchestrator`,
  `watchdog::tests::the_watchdog_waits_out_a_failed_starts_backoff`; `alondrad`
  `startup_tests::a_second_daemon_is_refused_before_its_restart_recovery_touches_the_live_daemons_deliveries`;
  `daemon-server` `preflight::tests::preflight_refuses_a_state_file_the_real_startup_cannot_read`.

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
- **Status**: fixed in `e7b898c30`. On 2026-11-23 the user decided that Alondra watches only the Mac's
  own disk (its system and data volume, the one that can stop nodes) and that learning build
  volumes goes whole, with no rule to forget them: F-ENGINE-7 and F-ENGINE-9 no longer apply, and
  F-ENGINE-6 and F-ENGINE-10 are fixed on that disk alone. Final behaviour:
  - a start holds its slot from its admission until its node exists or its launch ends
    (`holds_slot`), and an `auto_launch` admission that does not launch gives it back at once
    (`Admitted` guard);
  - the pause of a queued task is the domain fact `Task::paused_in_queue` (`PauseQueuedTask`,
    `ResumeQueuedTask`); its launch keeps its row in `launch_queue`, closing the task clears it, and
    a pause an older daemon kept in `admission.json` is handed to the domain once;
  - a turn given a slot keeps its age in `admission.json` (`Saved::granted`) until it is taken;
  - the disk incident counts only new samples (`pace` says whether the sample was new);
  - `disk_room_for` reads and decides under one lock;
  - the `Disk` port only reads a path's space; a launch reads the space of its worktree's folder
    once, and no other volume is watched, learned, reserved or named on a card.
  Tests, each failing without its fix: `admission::tests::a_start_whose_launch_outlasts_three_minutes_keeps_its_slot_until_the_launch_ends`,
  `auto_launch::tests::an_automatic_start_that_gives_way_gives_its_slot_back`,
  `queue_pause_tests::cancelling_a_task_paused_in_the_queue_ends_its_pause`,
  `admission::tests::a_queue_pause_an_older_daemon_kept_in_the_gates_file_becomes_the_domains`,
  `admission::tests::a_turn_given_a_slot_keeps_its_place_in_the_queue_across_a_restart`,
  `machine_watch::tests::a_second_look_at_the_same_sample_moves_no_recovery`.

### G5 · Every request and task has someone who moves it next

Issue: #522 · Severity: S2 · Findings: F-CARDS-1, F-CARDS-6, F-ORCH-2, F-ORCH-8, F-LIFE-1,
F-GH-5, F-CARDS-7 (added on 2026-11-22 by the user's decision, after the re-check of #536) · Depends on: none · Related: issues #353, #323, #350, #352.

**Status: fixed** in #553 (phase 5 of the fix plan). Each finding below says what
changed and which test proved it; every new test was seen failing with its fix removed.

- **Expected behaviour**: a request whose tasks all ended is reminded to the orchestrator at most
  twice, then Alondra closes it with its own «Done» card (user decision). `CreateTask` and
  `SetDependencies` refuse a dependency that is already cancelled, like `SetRelations` does. Replan
  reminders do not feed themselves: only rests caused by other work remind, and after K reminders the
  task shows a factual state (default decision). `create_task` carries its priority before
  auto-launch can wake. A task that waits for other tasks shows «waiting for tasks», never «queued»,
  because the aggregate records the dependency wait (default decision). «Clear completed» takes only what the list shows as completed: never a closed request one of whose tasks is still ending (F-CARDS-7). After a merge, a turn that
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
on: none · Related: issue #317 · **Status**: fixed in #551 (commit `60d638a0e`). The four findings
were reproduced on `main` at `10ebafced` by the tests of `crates/domain-rules/src/card_protocol_tests.rs`,
which fail without the fix and pass with it (ablation in the pull request).

- **Final behaviour**: replacing one question of a group is refused with `question_in_group`
  (field `card.replaces`), which tells the node to withdraw the group and ask it again whole (default
  decision). An approved plan is read-only (user decision): `SubmitFeedback` on it creates a request
  that names the plan card and its build (`reply_to` with the `implement-<plan>` request and its tasks):
  «request changes» a new request, «reply» and «question» a message to the orchestrator; the plan stays
  approved and its node goes on cleaning up. A new revision of a document hands back every final
  delivery that carried the old one, with a notice to its node, also a node of another task sharing the
  document (`withdraw_deliveries_behind_their_targets`, the PR sweep extended to documents). A card
  that replaces the current final delivery hands it back as `Withdraw` does
  (`hand_back_replaced_deliveries`, default decision), and `presentation()` refuses a withdrawn delivery.
- **Files**: `crates/domain-rules/src/rules.rs` (interactions, `replaces`, plan reviews, document
  revisions), `card_contract.rs`, `decisions.rs`.
- **Tests**: domain tests, one per rule, each with the exact sequence of its finding.

### G7 · GitHub: no closing, merging or reading against a stale picture

**Status**: FIXED (G7, PR #606, commit `eff59c21d`, from `main` at `93dbd23c8`). Each finding below carries its
test and ablation. The race F-GH-1 describes in the watcher's round was already closed on `main` by #590
(`9c599b4dc`), which reads the domain again after each PR's GitHub read; its test pins that. What still broke
the user's decision of 2026-11-22 was the hourly sweep, which closed again a PR the user reopened.

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
**Status**: F-ENGINE-12 and F-PROV-1 FIXED in #613; H-ENGINE-8 stays a hypothesis (see its entry). Both
findings were reproduced on `main` (`e1d5ec2aa`) by their new tests, failing, before the fix. Coordination: the
Codex recovery work (phase 2) had nothing on `main` and its pending changes touch the owner's handoff and the
Codex host's exit, not `take_host` or the adoption; this fix keeps `take_host` as it was and puts the exclusion in
`adopt_for_transition`, so the two do not overlap.

- **Expected behaviour**: the adoption after a model transition waits its turn and never takes a host
  a resume is taking; a stale take never reaches `lost()` or ends the other owner's host. A usage
  reading measured before a limit rejection never erases it: readings compare measurement time, and
  the probe fence covers `account_usage`.
- **Files**: `crates/engine-sdk/src/sdk_owner.rs` (`take_host`), the model-transition adoption,
  `crates/core/src/usage.rs`, `crates/daemon-engine/src/usage_probe.rs`, the `RecordAccountUsage` rule.
- **Tests**: an engine test with barriers that interleaves a resume and an adoption of the same host;
  a usage test applying a reading measured before a rejection after it.
- **Final behaviour**: `adopt_for_transition` takes the task's `TakeoverTurn`, looks again whether an owner of this
  daemon holds the host (`owned_now`) and, only if none does, takes it and adds its owner before letting the turn go;
  it returns whether an owner now carries the host. A retake under way therefore finishes first and the change goes
  on with the owner it left. `usage::record` keeps every window measured after the reading it applies: a reading
  never replaces a newer window of the same dimension, and a complete reading only removes windows measured before
  it. The probe fence does not cover `account_usage`: with readings ordered by measurement time a probe that
  overlaps a limit no longer needs to be repeated, and a fence on usage would repeat every probe that overlaps any
  turn's usage event.

### G9 · Terminal client and local pairing recover by themselves

**Status**: FIXED (issue #526). Each finding below carries its test and ablation. One part differs from the
expected fix written here: over the direct HTTP path only a refusal the Mac seals to the device counts as the
Mac's (F-CLIENTS-R2), because a bare HTTP status can come from whoever answers on that address.

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
- **Final behaviour**: the restart hands every Mac's unconfirmed answers to the new process in the restore
  file (`Restore::unconfirmed`), which sends them again at once with their own `command_id` and keeps their
  cards out; quitting by hand with answers on their way asks «Answers still on their way» first, and a second
  quit, or «quit anyway», quits. The Mac seals to the device the refusals it can prove (`busy`, `expired` for
  an envelope the device signed that arrived late, `idempotency_conflict`, `rejected`) and the terminal reads
  them over HTTP as it reads them over iroh; a bare status stays «no answer». Every request for the screen's
  view takes a ticket and an answer to an older request than the last one taken is dropped, `Ok` or `Err`;
  a view asked for before the daemon confirmed an answer cannot bring its card back. «device not paired»
  pairs the terminal again once per process and the TUI says so once. The device list leaves out this
  Mac's own terminal and `DELETE /devices/{id}` refuses it with `own_terminal`.

### G10 · Apple apps: optimistic actions and per-Mac state

**Status**: FIXED (issue #527). Each finding below carries its test and ablation. Two parts differ from the
expected fix written here: the iPhone queue half of F-CLIENTS-A5 was already fixed on `main` by `77d49758b`
(#533), so only the Mac sidebar changed; and a push wakes the answers kept for every paired Mac, since the
push callback does not say which Mac it came from (F-CLIENTS-A1).

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
- **Final behaviour**: an answer `sendNow` keeps for later starts its own background delivery at once
  (`ViewActionSender.startDelivery(_, failedSince:)`), which waits its turn and goes as soon as anything wakes
  it, whether or not a screen of that Mac is open; a push wakes every paired Mac's sender
  (`AccountSession.macReached`). Tasks, the task detail and Scheduled tasks start a 30-second confirmation
  timer per sent action: when it runs out the hidden row or button shows again, the toast says what has
  not reached the Mac and why, from the app's single connection state (`LateNotice`, cause first so a long name
  never hides it: «No internet: still sending “Cancel Task”.», «Connecting to your Mac: still sending…», or
  «Your Mac hasn’t confirmed it yet: still sending…» when it is connected), and the delivery goes on; a late confirmation
  hides it again. The card queue's 30-second notice on iPhone says the same. Every failed delivery try logs
  its cause (error kind and code, never what was written) in the app log the app report reads
  (`answer-delivery`). A message to a task's node shows nothing while it is retried, by the user's decision
  of 24 Nov 2026: the field clears once the message is saved on the phone, and only a real failure brings
  the text back with why. QR pairing
  asks the account for the access token on every request (`PairingAuthorization.bearer`), tells it when the
  relay refused one, and the approval poll waits and asks again after a cut, a 401, 408, 429 or 5xx until
  the QR expires. The Live Activity treats a report from one Mac as the end of the previous one being
  followed: its numbers keep the time they were loaded and a newer sealed report replaces them. A request
  that could not be prepared on this device emits `.settled(applied: false)` before `.undelivered`: the Mac
  sidebar stops counting it as sending, keeps the card out until the daemon's next sidebar as it does for
  any answer that did not go through, shows the reason on the card, and never says the Mac did not confirm
  it.

### G11 · Devices, relay and self-update

**Status**: FIXED (issue #528) for F-SERVER-D1, F-SERVER-D5, F-SERVER-D7, F-SERVER-R4 and F-SERVER-R6, each with its
test and ablation below, validated in #628. F-SERVER-R5 stays a hypothesis: no test could confirm it (see
there). One part differs from the expected fix written here: an enrolment without a settling answer is sent again
with the same device ID instead of being deleted first, because a signed DELETE marks the ID revoked at the relay
for good, while a repeated POST of the same device, key, owner and Mac is answered as the enrolment it already has.

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
- **Final behaviour**: a `pending` enrolment of a still approved device is sent again on the next round until the
  relay answers; a 409 or 400 from the relay leaves it `refused`, never sent again, and the device list says «the
  relay refused it: pair it again from the device» (`relay_refused` in `GET /devices`). A direct pairing request
  from an ID this Mac revoked, or that it holds with other keys, is refused at `start` (`pairing_denied` /
  `pairing_conflict`) and never listed; one that becomes so while it waits ends `denied` when approved. A failed
  `brew upgrade` is tried again 30 min, 2 h and 8 h later, then waits for the next version, and `alondra doctor`
  says which. The relay forgets revoked reports at start and on every one-minute round, and a start it recorded
  but stopped before APNs answered counts as never sent when it opens its file.

### G12 · The architecture docs match the code

Issue: #529 · Severity: S3 · Findings: F-ENGINE-5, F-ENGINE-11, the differences listed at the
end of the lifecycle area, and the gaps of `crates/machine-model` · Depends on: G2, G4 and G5 (it
documents their final behaviour).

- **Expected behaviour**: `docs/architecture/task-states.md` describes the timers that exist (and
  only those), places `Situation` where it lives, includes `Replan` in the obligation table, and stops
  claiming «no known gaps» for the model test; the hook transport's docs describe only reachable
  states; the model test covers interruptions, reopening and Codex, or lists them as gaps.
- **Files**: `docs/architecture/task-states.md`, the hook transport docs, `crates/machine-model`.
- **Status: fixed** in #624, checked against `main` at `b479c4f7c`, after G2 (#552), G4 (#556) and G5 (#553).
  `task-states.md` lists the two deadlines of `obligation_policy` and every engine timer outside it with why it
  is not there (F-ENGINE-5), places `Situation` in `crates/domain-rules/src/situation.rs` with its real
  precedence, adds `Replan` and `obligation_of` to the obligation table, states rules 3 and 4 as the code holds
  them, and lists what the model test does not drive (interruptions, reopening, Codex and the rest of
  M-LIFE-14) instead of «no known gaps». The hook transport's unreachable held messages and capped-turn card are
  removed from the code and the comments say where a capped node goes (F-ENGINE-11). Evidence: review against
  the code at `b479c4f7c` (each item in [Docs that differ from the code](#docs-that-differ-from-the-code) says
  what changed), and the Bazel build and tests of the changed crates.

## Referred to the tasks that own them

These findings fall inside another task's scope. The evidence is handed to that task; no
parallel fix is planned here.

| Finding | Owner | New evidence for the owner? |
|---|---|---|
| F-PROV-2 · reconciling an order the host never had, past its deadline, destroys the socket | the Codex recovery work (phase 2), point 8 | Yes: the exact path and the false «host lost» card |
| F-PROV-4 · a continued host refuses `start` without a typed code | the Codex recovery work (phase 2), point 7 | No |
| F-PROV-3 · Claude's checkpoint carries a dead host's open decisions and calls; at 64 a new decision is dropped | issue #328 and the stalled-node recovery work (phase 3) | Yes: the sets `forget_ended_host` keeps and the silent drop at the cap. Since #537 the `pr_commands` set is gone (read and dropped from older checkpoints); `pr_close_commands` and `asked` remain |
| H-LIFE-1 · a repeated `TurnStarted` after its `TurnEnded` reopens the turn | the stalled-node recovery work | Yes: the sequence and the guard that swallows the end |
| F-ENGINE-5 (part) · `opening_deadline` decides a host replacement outside the obligation policy | the stalled-node recovery work | Yes. Since #541 (at `68770a298`) it is a constant of its own, `OPENING_TIMEOUT` = 120 s (`engine-sdk/src/sdk_owner.rs:53`), no longer `io_timeout_seconds`, and past it the owner first reads what the host already wrote (`:2298-2306`). Still outside `obligation_policy`; since G12 `task-states.md` lists it as an exception owned by that work |
| M-ORCH-6, H-ORCH-3 · the orchestrator's `Answer` deadline | the stalled-node recovery work (phase 4, `AnswerSkipped`) | Yes: #516 already removed that deadline (at `68770a298`); what the orchestrator owes is settled only after a turn of it ends handled (M-ORCH-5), so while it cannot end one (F-ORCH-3) nothing settles a node's question or a stopped node's case |
| H-PROV-2 · an input "offered" to Codex is only queued in the host | the provider adapters work (phase 3), point 10 | No |
| F-PROV-1 (variant) · usage readings an ended host's journal kept are committed on adoption with the commit time, so an old one can overwrite a newer reading | the provider adapters work: the host stamps each journal event with its time and the Claude and Codex adapters read it | Yes: `OrderedObservation` carries no event time; `store-sqlite/src/owner.rs` records `ObservedFact::Usage` with `at = now`. Referred by the user on 2026-11-24 |
| H-GH-1, H-GH-2 · identity of moved pull requests | the stable pull request identity work | See the hypotheses in the GH area. #537 touches neither (`RegisterPr` and `ObservePrState` are unchanged at `68770a298`) |
| H-GH-6 · a branch PR noted for a task and registered meanwhile by another live task blocks that task's final delivery for good | the stable pull request identity work | Yes, new at `68770a298` (see the hypothesis in the GH area) |
| #537 · a PR enters a task only through `register_pr` or GitHub's open PR of one of its worktree branches at a final delivery | the stable pull request identity work | Yes: a PR the node opens from a checkout that is not one of its task's worktrees and does not register is never known to Alondra, and nothing asks about it any more (`AskToRegisterPr` is inert, `domain-rules/src/rules.rs:4855-4858` at `68770a298`); before, the output of `gh pr create` noted it or asked the node. A design choice of #537, listed so the identity work weighs it |
| F-STORE-2 (part) · `--preflight` does not exercise the JSON state files startup needs | the tests-by-layer work (host model), point 30 (binary smoke test) | Yes: the list of files startup refuses on |

F-ENGINE-12 (two owners take the same host) is fixed in group G8 (#613) without changing `take_host`, the
function the Codex recovery work (phase 2) changes: the exclusion lives in `adopt_for_transition`.

## Product decisions

Decided by the user on 2026-11-22, on cards of this audit:

| Question | Decision |
|---|---|
| May Alondra delete, after 30 days, files of its `files/` folder that it did not create? (PR #503 deletes every top-level entry outside its known folders) | **Delete only inside its known folders** (tasks, results, uploads, images, documents). Anything else is listed in the retention report and never touched. |
| What do «request changes» or «reply» do on a plan already approved? (F-CARDS-3) | **An approved plan is read-only.** «Request changes» becomes a new request; «reply» reaches the orchestrator as a note on the build in progress, without stopping it. |
| If the user reopens on GitHub the pull request of a cancelled task, does Alondra close it again? (F-GH-1, open PR sweep) | **Close it once, then hand it to the user.** Alondra closes the PR once when the task is cancelled; if the user reopens it, Alondra respects that and shows it as an open PR for the user to decide. |
| Should Alondra learn the other volumes builds write to and watch them? (F-ENGINE-9, decided 2026-11-23) | **No: it watches only the Mac's own disk**, its system and data volume, the one that can stop nodes. Learning build volumes, the reservations on them, their states and their files are removed; there is no rule to forget a volume because none is learned. |
| Who closes a request whose tasks all ended, if the orchestrator does not? (F-CARDS-1, issue #353) | **Remind the orchestrator up to twice, then Alondra closes it** with its own «Done» card, without the orchestrator's summary. |

Defaults applied from rules the user has already set (optimistic UI with a notice after 30 s; no
technical cards for the user; a task only waits for capacity or the user; every request ends
with an outcome card; several Macs per client). Each can be changed in the review of this file:

| Finding | Default |
|---|---|
| F-LIFE-1 · where the wait for other tasks lives | In the lifecycle aggregate, as the single source: `AwaitingTasks`, recorded with `WaitsForTasks`/`TasksLanded` (the user confirmed it on 2026-11-23). |
| F-LIFE-2 · reopening a task whose SDK node the user kept open | Reopen without a node; the gate starts a new SDK execution that resumes the saved session. |
| F-CARDS-2 · replacing one question of a group | Refused: the node replaces the whole group with another group. |
| F-CARDS-5 · replacing the current final delivery with a card that is not a final delivery | Same effects as `withdraw`: the task goes back to in progress (while #317 keeps clarifications). |
| F-GH-5 · post-merge work and an idle node | Enforce the documented rule: after the merge, a turn that ends with nothing running and without declaring the work again ends the post-merge work. |
| Cancelling while `gh pr merge` runs | Accept the cancellation and say on its outcome that the merge was already in progress. |
| F-ORCH-1 · a batch cut by a daemon restart | Alondra resumes alone: confirmed effects are handed off, a batch with no effect is offered again, and an uncertain operation goes to the next batch as evidence. No card for the user. |
| F-ORCH-2 · reminders of a replan the orchestrator does not decide | Remind only on rests caused by other work; after K reminders the task shows the factual state «waiting for a decision of the orchestrator» and no more batches until something changes. |
| F-PROV-1 · a usage reading and a limit rejection disagree | The latest measurement wins, by measurement time (not arrival time). |
| F-PROV-2 · an expired order the host never had | Typed `expired` rejection; the input goes back to the queue without a card. |
| Engine deadlines outside `obligation_policy` (F-ENGINE-5, F-ENGINE-1) | `opening_deadline` moves into the policy, with measure and reason; `SILENT_HOSTS` stays in `sdk_owner.rs` as a documented exception, because it counts ended hosts and is not a time a step may take; `UNOWNED_TAKE_EVERY` stays a cadence; `IN_FLIGHT` stops deciding slots and only feeds the memory estimate. |
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

```toml state-machine
id = "M-LIFE-1"
code = ["crates/domain/src/lifecycle.rs::TaskLifecycle", "crates/domain/src/lifecycle/commands.rs::transition", "crates/domain/src/lifecycle/observations.rs::runtime_fact", "crates/domain/src/lifecycle.rs::advanced", "crates/domain/src/lifecycle.rs::CallbackState", "crates/domain/src/lifecycle.rs::State", "crates/domain/src/lifecycle.rs::TaskPhase"]
truth = ["sqlite:lifecycle_events", "sqlite:lifecycle_cards", "sqlite:lifecycle_tasks", "sqlite:task_user_inputs"]
states = "crates/domain/src/lifecycle.rs::TaskPhase"
variants = ["Unstarted", "InProgress", "Delivered", "Finishing", "Completed", "Cancelled"]
terminals = ["Completed", "Cancelled"]
initial = ["Unstarted", "InProgress"]
transitions = ["Unstarted -> InProgress", "Unstarted -> Finishing", "InProgress -> Delivered", "InProgress -> Finishing", "Delivered -> InProgress", "Delivered -> Finishing", "Delivered -> Completed", "Finishing -> Completed", "Finishing -> Cancelled", "Completed -> InProgress", "Cancelled -> InProgress", "Completed -> Delivered", "Cancelled -> Delivered"]
```

- **Kind**: explicit, composite. **Defined in**: `crates/domain/src/lifecycle.rs:1099-1196` (`Progress`, `DeliveryGate`, `Delivered`, `Finishing`, `State`, `TaskLifecycle`); transitions in `lifecycle/commands.rs:4-568` (`transition`) and `lifecycle/observations.rs:4-719` (`runtime_fact`); `advanced` in `lifecycle.rs:1668-1704` (copies, applies, bumps the revision, sets `owed_since`).
- **Source of truth**: the task journal (`DomainState.lifecycle_journal: Vec<LifecycleRecord>`, persisted in `lifecycle_events`, `crates/store/src/schema.sql:13-19`). The aggregate is rebuilt by replay (`domain-rules/src/lifecycle.rs:1240-1247`, `lifecycle_of`) and cached in `DomainState.lifecycles` (Arc; used only if `revision+1 == journal.len()`).
- **Copies and derivatives**: projections the store writes: `lifecycle_tasks.revision`, `lifecycle_current_execution`, `lifecycle_cards`, `lifecycle_dependencies`, `lifecycle_delivery` (`crates/store/src/persistence.rs:191-245`). Copies in the `tasks` row (see M-LIFE-6).
- **States** (`State`):

| State | Content | Notes |
|---|---|---|
| `Unstarted{input, remaining}` | no execution | |
| `AwaitingTasks{tasks≠∅, start}` | no execution; `start` is `First{input, remaining}` (phase `Unstarted`) or `Again{stage, input, remaining}` (phase `InProgress`) | the wait of a task no execution runs for the tasks it may not start before; owes `Tasks` (or the plan review's `Answer`); `Admit` refuses it with `AwaitsTasks` (F-LIFE-1) |
| `InProgress(Queued{stage,input,remaining})` | no execution | |
| `InProgress(Executing{stage, ActiveExecution})` | execution present, `Available` or `Reserved+Launching` | `ActiveExecution::new` rejects `Resting` (`lifecycle.rs:1063-1077`) |
| `InProgress(Waiting{stage, Decisions≠∅, Session})` | non-empty cards by kind (`Decisions`) | |
| `InProgress(Dependency{stage, tasks≠∅, decisions, Session})` | | with `Session::Absent`: an `AwaitingTasks` task asked a card. With an execution: **unreachable in production**, no record makes a running task wait for its dependencies |
| `Delivered{delivery, gate, decisions, Session}` | `gate ∈ {Review, Blocked, AwaitingMerge, Accepted}` | `Blocked` and `AwaitingMerge` are **unreachable in production** (no `BlockDelivery` record, no `Accept{merge:Some}`) |
| `Finishing{intent, remaining≠∅, Session}` | drain obligations | |
| `Completed` / `Cancelled` | terminal | |

- **Composite state**: `State × Execution (M-LIFE-2) × plan_reviews × post_merge × doubts/settled × empty_turns/woken`. The fields outside `state` (`plan_reviews`, `post_merge`, `doubts`, `settled`, `cancelled_through_generation`) survive `Reopen`.
- **Events** (`Event`, `lifecycle.rs:513-712`): Admit, Runtime{stamp,cursor,fact}, Ask, Answer, Decide, MoveCard, KeepQuestion, PlanApproved, HostReleased, HostRetaken, BlockDelivery, DeliveryUnblocked, Present, Accept, MergeObserved, HandBack, WaitForDependencies, Tell, DependenciesResolved, DependenciesLanded, Pause, Resume, EndPause, RecoveryEnded, Finish, DrainObserved, PostMergeDeclared, PostMergeEnded, RetireReleasedExecution, ExecutionReleased, ExecutionLeftForReview, Reopen, HoldForPlanReview, PlanReviewSettled, DoubtHandoff, ResendUntaken, ResendDoubtful, SettleHandoff, Discard, AwaitEnded, Unfinished, RequestControl, Compact.
- **Events produced by `domain-rules`** (mapping `event()`, `domain-rules/src/lifecycle.rs:616-803`): all except `Ask`, `PlanApproved`, `BlockDelivery`, `DeliveryUnblocked`, `MergeObserved`, `Accept{merge:Some}`, `DependenciesResolved`, `RetireReleasedExecution`, and the facts `ConnectionLost`, `ResourceReleased`, `Notification`, `EffectAccepted`, `EffectObserved` (checked with `rg`: 0 occurrences in `domain-rules/src/{lifecycle.rs,rules.rs,rules/}`). These are aggregate branches that only its own tests exercise.
- **`Compact` since #507** (at `3fc0370ea`): no command writes a new `Compacted` record any more
  (`Command::CompactConversation`, its guard and the idle-compaction loop were removed). The record and its event stay
  so older journals replay: `LifecycleRecord::Compacted` (`domain-rules/src/lifecycle.rs:474-476`) still maps to
  `Event::Compact` (`:770`) and introduces the input `/compact` (`:851`); the aggregate branch is unchanged
  (`domain/src/lifecycle/commands.rs:546-566`: only `Waiting` or `Delivered`, resource `Available`, resting, no
  `Pause` card). A `Compacted` committed by an older release whose input was still pending keeps its staged dispatch
  (`store-sqlite/src/repository.rs:1077`), so that one input still reaches the host after the upgrade: an ordinary
  turn, no invalid state.
- **Allowed transitions (essential)**:

| From | Event [guard] | To / effect |
|---|---|---|
| Unstarted, Queued | WaitForDependencies (record `WaitsForTasks`) | AwaitingTasks{tasks, start} |
| AwaitingTasks, Dependency | WaitForDependencies [another set] | the same state with the new set |
| AwaitingTasks | DependenciesLanded (record `TasksLanded`) | Unstarted/Queued as `start` says |
| Dependency{Absent} | DependenciesLanded | Queued, or Waiting with its cards |
| AwaitingTasks | Ask, Pause | Dependency{Absent} with the card |
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
| Completed/Cancelled | `AdoptOpenPr` (system) [its PR is open again and no live task has the branch] | Reopen and, in the same command, the PR is delivered again (`settle_open_pr`): Delivered{Review} for the user (`rules.rs:5474-5510`) |
| Executing/Waiting/Dependency/Delivered with an execution | ExecutionReleased / ExecutionLeftForReview [no work, callbacks or effects] | Session::Absent with what was not consumed |

- **Rejected**: every `Err(Refusal::…)` is typed; `apply` leaves the aggregate untouched (`lifecycle.rs:1662-1666`). In `domain-rules` a refusal becomes a `DomainError` (`refused`, `lifecycle.rs:3198`) and aborts the whole command. Facts on terminal states: `runtime_fact` returns `StaleExecution` because there is no session (`observations.rs:10`). In `Finishing`, every fact other than `HostEnded/HostStopped/TakenOver/TakeoverUnanswered` is taken "unchanged" (only the cursor advances, `observations.rs:21-24`).
- **Terminals**: `Completed`/`Cancelled`. Only `Reopen` reopens them, and only two explicit commands emit it (`Command::ReopenTask`, `rules.rs:5296-5315`; adoption of the PR of a closed task, `rules.rs:5260-5294`). No late provider fact or answer reopens them: without a session `runtime_fact` refuses and `answer()` refuses (`commands.rs:684-698`). Since G2 (#552) `ReopenTask` never re-admits the old node: the reopened task has no node and no execution, and its next SDK execution comes from the gate.
- **Invariants**: enforced by type: `Decisions` non-empty, `NonEmpty` in dependencies and drain, `Several` in `LiveState`, `ActiveExecution` (not `Resting`), `TaskLifecycle` without `Clone`/`Deserialize` (`compile_fail` doctest). Only assumed: that `Executing` never ends up with `Activity::Resting` (imposed by the `runtime_fact` paths and `ActiveExecution::new`, not by the type; `Tell` on `Executing` mutates `execution.0` without revalidating, `commands.rs:174`).
- **Persistence and restart**: the journal is the truth; after a restart `load_journal` rebuilds, validates each task with `agrees` (`domain-rules/src/lifecycle.rs:1351-1480`), and loading fails if a row does not match. The aggregate's cursor is recomputed on replay (`event()` always uses `current cursor + 1`, `lifecycle.rs:617-620`): the domain journal stores neither the provider's cursor nor its stamp; deduplication of repeated facts lives upstream (cursor of `provider_observations`, see M-STORE-5) and in `turn_end_recorded` for turn ends.
- **Concurrency**: the aggregate is pure; only `DomainState::decide` mutates it, inside a command, and the store serializes commands (out of scope, see STORE).

### M-LIFE-2 · Execution (`Execution`)

```toml state-machine
id = "M-LIFE-2"
code = ["crates/domain/src/lifecycle.rs::Execution", "crates/domain/src/lifecycle.rs::queue_input", "crates/domain/src/lifecycle/observations.rs::runtime_fact"]
truth = ["derived", "sqlite:lifecycle_current_execution", "sqlite:lifecycle_execution_bindings", "sqlite:lifecycle_executions"]
```

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

```toml state-machine
id = "M-LIFE-3"
code = ["crates/domain/src/lifecycle.rs::TurnStop"]
truth = ["derived"]
states = "crates/domain/src/lifecycle.rs::TurnStop"
variants = ["Running", "InterruptAccepted", "ForceRequested"]
```

- **Kind**: explicit. **Defined in**: `lifecycle.rs:311-316`.
- **Transitions**: `Running → InterruptAccepted` by fact; `InterruptAccepted → ForceRequested` by `RequestControl{ForceStop}` (`commands.rs:534-546`).
- **Rejected**: `ForceStop` on `Running` → `InvalidState`; repeated → `DuplicateObligation`.
- **Persistence and restart**: lost when the turn ends (it is a field of `Activity::Turn`).

### M-LIFE-4 · Empty-turn counter

```toml state-machine
id = "M-LIFE-4"
code = ["crates/domain/src/lifecycle.rs::wake_with", "crates/domain/src/lifecycle.rs::rest_wakes"]
truth = ["derived"]
```

- **Kind**: implicit. **Defined in**: `empty_turns: u8`, `woken: Option<InputId>` (`lifecycle.rs:1191-1194`); rises in `wake_with`, resets in `rest_wakes` except for the `keeps_wake` events (`lifecycle.rs:1707-1735`). Limit `EMPTY_TURN_WAKES = 3`.
- Decides whether an empty turn end wakes the node or opens the orchestrator case (`hands_back_an_empty_turn`, `lifecycle.rs:1230-1242`; `domain-rules/src/lifecycle.rs:1598-1645`).
- **Source of truth**: the aggregate alone (rule 4 of the doc holds for the count).

### M-LIFE-5 · Obligation (`obligation()`)

```toml state-machine
id = "M-LIFE-5"
code = ["crates/domain/src/lifecycle/obligation.rs::obligation"]
truth = ["derived", "sqlite:api_failures"]
```

- **Kind**: derived. **Defined in**: `lifecycle/obligation.rs:53-161`, `Party × Owed`, `since = owed_since` (revision at which it changed).
- **Total**: for every non-terminal state it returns `Some`: `Waiting` carries non-empty `Decisions` and `waits_on` covers the three `Owner`s; `Delivered` falls to `gate`; `Executing` falls to `by_activity` (which covers every `Activity`). `Completed/Cancelled` → `None`. Checked, no gaps.
- **Adjustments outside the aggregate**: `DomainState::obligation_with` (`ownership.rs:329-342`) changes the obligation to `Answer` (returned delivery waiting for the orchestrator) or `Replan` (cancelled dependency / rejected plan): the aggregate never produces `Owed::Replan`.
- **Deadlines**: `obligation_policy.rs` (`HAND_INPUT` 5 min, `WAKE` 2 min, `ANSWER` 10 min), applied by `obligation_watch` (out of scope: ENGINE).
- **Since #516** (at `68770a298`): `ANSWER` is gone; only `HandInput` and `Wake` have a deadline and are enforced (`domain-rules/src/obligation_policy.rs:10-27`). `Owed::Answer` and `Owed::Replan` stay as obligations (the task still reads as waiting for the orchestrator) but no clock acts on them: what the orchestrator owes is settled by the end of its handled turns (M-ORCH-5).

### M-LIFE-6 · Task row `tasks.state` and flags

```toml state-machine
id = "M-LIFE-6"
code = ["crates/domain-rules/src/model.rs::TaskState", "crates/domain-rules/src/rules.rs::derive_states", "crates/core/src/tasks.rs::TaskState"]
truth = ["sqlite:tasks", "sqlite:task_cancel_reason", "sqlite:task_history", "sqlite:task_projects"]
states = "crates/domain-rules/src/model.rs::TaskState"
variants = ["NotStarted", "Running", "NeedsYou", "AwaitingBlocker", "ForYourReview", "WaitingDependency", "Closing", "Done", "Cancelled"]
terminals = ["Done", "Cancelled"]
```

- **Kind**: implicit, derived with validation. **Defined in**: `domain-rules/src/model.rs:123-145` (`TaskState` with 9 values), `Task` (`started`, `agent_id`, `final_presentation`, `ending`, `wrap_up`, `pending_user_inputs`, `waiting_for_tasks`, `dependencies`, …).
- **How it is written**: (1) `materialize` after every lifecycle decision sets `agent_id`, `started`, `ending` and the phase's `TaskState` (`domain-rules/src/lifecycle.rs:1314-1336`); (2) `derive_states` / `state_within_phase` picks the attention state within the phase (`rules.rs:2614-2662`, `lifecycle.rs:2613-2650`); (3) `derive_loaded_states` keeps `WaitingDependency` on open (`rules.rs:2626-2636`).
- **Validation**: `agrees` (`lifecycle.rs:1413-1480`) for every task "touched" in the commit (`rules.rs:3012`) and on load: node/start/ending equal to the execution; phase ↔ state compatible; required cards actionable.
- **Copies that can diverge**: `dependencies`/`waiting_for_tasks` (row) against `Progress::Dependency` (aggregate, dead) and `Await::Tasks` (aggregate, only after a turn): the dependency wait of a task no execution runs is `AwaitingTasks` in the aggregate, recorded from the row's relations by `record_start_waits` at the end of every command (F-LIFE-1, fixed). `wrap_up.post_merge_work` (row) against `post_merge` (aggregate): synchronized in `declare_post_merge`/`end_post_merge`/`adopt_declared_post_merge` (`lifecycle.rs:2441-2491`).
- **Terminals**: `Done`/`Cancelled` only from the phase; `derive_states` does not touch terminal rows.

### M-LIFE-7 · `Situation` and `Tally`

```toml state-machine
id = "M-LIFE-7"
code = ["crates/domain-rules/src/situation.rs::Situation", "crates/domain-rules/src/situation.rs::Tally"]
truth = ["derived"]
states = "crates/domain-rules/src/situation.rs::Situation"
variants = ["Working", "Waiting", "NeedsYou", "Paused", "Delivered", "Finished", "Cancelled"]
terminals = ["Finished", "Cancelled"]
```

- **Kind**: derived. **Defined in**: `domain-rules/src/situation.rs:7-226`; computed by `ReadIndex::build` per task (`read_index.rs:614`) with `StartProblems::situation` → `situation_with`.
- **Reads**: `task.state` (terminals), the inbox listing, `task_activity` (closed node), `node_closing`, `obligation_of` (M-LIFE-5), cards and `awaits` of the aggregate, `answered_wait`, `task_handed_back`, `finishing_intent`. `Tally` decides block and counters (`situation.rs:68-82`, `read_index.rs:99`, `views/src/lib.rs:85-93`, `views/src/tasks.rs:530-540`; `:545-555` at `3fc0370ea`). Which tasks enter those blocks at all is M-CARDS-13 (since #536 every task that has not ended does).
- **Real precedence**: terminal row → inbox `needs_you` → closed node (`Waiting(Slot)`) → delivered and node closing → turn/work → user pause or `Await::Instruction` → user cards → delivered phase → cancellation in progress → wait for an obligation.

### M-LIFE-8 · `LiveState` / `TaskActivity` / node state in `/runtime`

```toml state-machine
id = "M-LIFE-8"
code = ["crates/domain/src/lifecycle.rs::LiveState", "crates/domain-rules/src/node_machine.rs::TaskActivity", "crates/domain-rules/src/lifecycle.rs::live_state", "crates/core/src/model.rs::NodeState", "crates/core/src/model.rs::NodeStatus"]
truth = ["derived"]
states = "crates/domain/src/lifecycle.rs::LiveState"
variants = ["Paused", "Working", "WaitingUser", "WaitingCapacity", "WaitingTask"]
```

- **Kind**: derived. **Defined in**: the aggregate's `LiveState` (`lifecycle.rs:1499-1530`), adjusted by `DomainState::live_state` (`domain-rules/src/lifecycle.rs:1848-1866`) and wrapped in `TaskActivity` (`node_machine.rs:226-270`).
- **Copies and derivatives**: `ReadIndex` keeps `situation`, `activity` and `live` per task side by side (`read_index.rs:615-619`). `daemon-engine/src/live.rs:403-424` translates `sdk_live` into the inherited model's `NodeState`/`NodeStatus` for the node list: every `Working(Alondra(_))` (starting, delivering, waking, waiting for the orchestrator, merging, finishing) comes out as `Working`/`Busy`, while `Situation` treats it as waiting (see H-LIFE-3).

### M-LIFE-9 · Node phase (`NodePhase`) and node pause

```toml state-machine
id = "M-LIFE-9"
code = ["crates/domain-rules/src/node_machine.rs::NodePhase", "crates/domain-rules/src/node_machine.rs::Pause", "crates/domain-rules/src/node_machine.rs::node_next"]
truth = ["sqlite:agents", "sqlite:task_released_nodes"]
states = "crates/domain-rules/src/node_machine.rs::NodePhase"
variants = ["ThroughSdk", "Closed"]
terminals = ["Closed"]
initial = ["ThroughSdk"]
transitions = ["ThroughSdk -> Closed"]
```

- **Kind**: explicit. **Defined in**: `node_machine.rs:104-190`, `node_next` (`:340-373`).
- **States**: `NodePhase ∈ {ThroughSdk, Closed}` (any old value reads as `Closed`). `Pause{reason, since, cut, turn_since, cleanup ∈ {Pending, Terminated, Done}, released}`.
- **Events**: `SessionClosed`, `Paused`, `PauseCleanup`, `PauseReleased`, `Resumed`. The node's pause is mirrored as a `Pause` card in the aggregate (`hold_node_pause`, `:607-613`); its end decides `PauseEnded` or, if the aggregate refuses or the task would be left stranded, `Resumed` with text (`end_of_pause`, `:579-589`).
- **Terminals**: `Closed` is terminal for the node; `node_next` rejects everything except a repeated `SessionClosed`.

### M-LIFE-10 · Node closing / wrap-up

```toml state-machine
id = "M-LIFE-10"
code = ["crates/domain-rules/src/model.rs::WrapUp", "crates/domain-rules/src/model.rs::NodeEnd", "crates/domain-rules/src/rules.rs::node_closing"]
truth = ["sqlite:task_wrap_ups", "sqlite:node_last_words", "sqlite:task_stalls"]
```

- **Kind**: implicit. **Defined in**: `Task.wrap_up` (`asked_at`, `cleaned_at`, `node: Option<NodeEnd>`, `post_merge_work`, `close_cancelled`) + `node_closing()` (`rules.rs:2540-2546`: terminal, `wrap_up.node` or confirmed cleanup) + `in_cleanup`, `wraps_up`.
- Governs rule 8 of the doc (nothing asks, nobody resumes it). `RecordNodeEnd` drains everything at once if the task ends (`rules.rs:5409-5414`), or releases the execution if not.
- **Re-checked after #505** (at `3fc0370ea`): the domain side is unchanged (`node_closing` `rules.rs:2552-2554`,
  `RecordNodeEnd` `:5376-5432`). What changed is when the engine asks for it: the SDK owner's stop now waits for
  the input or control it is handing to the host to get its receipt before the host is told to end (M-ENGINE-10).
  A close therefore no longer leaves, by itself, the uncertain receipt that made the store refuse the end of an
  ended task (`UnresolvedDispatch`, `store/src/lib.rs:1548-1554` with `dispatch_holds_execution` `:1657-1660`). An
  uncertain receipt can still come from a host that dies on its own mid-handoff, from a handoff that times out
  (`io_timeout`), or from a second live owner of the same node (F-ENGINE-12, branch 2); then the closer's retry
  and its card after `MAX_FAILURES` apply as before.

### M-LIFE-11 · Held plan review

```toml state-machine
id = "M-LIFE-11"
code = ["crates/domain/src/lifecycle.rs::plan_revision", "crates/domain/src/lifecycle.rs::TaskLifecycle"]
truth = ["derived"]
```

- **Kind**: implicit, in the aggregate. **Defined in**: `plan_reviews: BTreeMap<InteractionId, u64>` + `plan_revision` (`lifecycle.rs:1178-1183`).
- `HoldForPlanReview` only before admission; `Admit` is refused while any is held (`commands.rs:12-14`). Outside `state`: survives `Finish` and `Reopen`.

### M-LIFE-12 · Doubtful deliveries

```toml state-machine
id = "M-LIFE-12"
code = ["crates/domain/src/lifecycle.rs::TaskLifecycle", "crates/domain/src/lifecycle.rs::Decisions"]
truth = ["derived"]
```

- **Kind**: implicit, in the aggregate. **Defined in**: `doubts: card → input`, `settled: set<input>` (`lifecycle.rs:1185-1189`).
- Events `DoubtHandoff`, `SettleHandoff`, `ResendUntaken`, `ResendDoubtful`; `Finish{Cancel}` moves the doubts to `settled`.

### M-LIFE-13 · Inherited reducer of `crates/core`

```toml state-machine
id = "M-LIFE-13"
code = ["crates/core/src/tasks.rs::TaskState"]
truth = ["memory"]
states = "crates/core/src/tasks.rs::TaskState"
variants = ["NotStarted", "WaitingDependency", "Running", "NeedsYou", "InReview", "Closing", "Done", "Cancelled"]
terminals = ["Done", "Cancelled"]
```

- **Kind**: explicit, in memory. **Defined in**: `core::tasks::TaskState` (8 values, `tasks.rs:74-122`) with the matrix `Board::apply_fact` (`tasks.rs:749-773`) and `core::states` (table `NODE_TABLE` of node states by hook evidence).
- **Source of truth**: lives only in memory in `Live.view` (`daemon-engine/src/live.rs:31-37`) for nodes seen through hooks (`claude --bg`); SDK nodes are overwritten from the lifecycle (`from_lifecycle`). A parallel source of truth only for non-SDK nodes; `client-port/src/i18n.rs` still imports its enums.
- **Persistence and restart**: not persisted; rebuilt from hooks after startup.

### M-LIFE-14 · `crates/machine-model` (model test; not a machine)

```toml state-machine
id = "M-LIFE-14"
code = ["crates/machine-model/src/lib.rs"]
truth = ["memory"]
```

- Models, with the real domain: deliveries, questions to the user and to the orchestrator, permissions, background work, PR, empty turns, host death and stop, late and repeated facts (deduplicated by the model's own cursor, `lib.rs:423-447`), restarts, slots, memory, disk, usage, account move, priority, cleanup.
- **Does not model** (0 occurrences in `lib.rs`): interrupt and `ForceStop`, `ReopenTask`, `HostReleased/Retaken`, `InputDropped`, `InputHeld`, `Unprompted` turns, compaction (since #507 only the replay of an older `Compacted` record can produce it, see M-LIFE-1), held plan review (`SettlePlanReview`), `WaitForTasks`, Codex, `RecordClaudeHold`, `DecisionAborted`, work after the merge (`DeclarePostMergeWork`), merge authorization. Nor tasks created with unstarted dependencies except `history_with_a_dependency`, which checks neither block nor counter (see F-LIFE-1). `KNOWN_GAPS` is empty (`tests.rs:14`), and the doc says "no known gaps" while admitting that it reimplements the gate and the host owner.
- **Since #516** (at `68770a298`): the model's orchestrator ends a handled turn every ten minutes of its clock unless the usage limit holds it, and reads every reminder (`orchestrator_inbox::owed(&state, &|_| true)`, `machine-model/src/lib.rs` `turn_ended`). It never models an orchestrator that cannot end a turn handled (failing launches, an ambiguous batch), so the gap of F-ORCH-3 is outside what it checks.
- **At G12** (`b479c4f7c`): the model now drives merge authorization (`merge_authorized`: claim, confirm, observe
  merged) and calls the pure decisions of the gate, the owners and the move (`admission::turns_to_grant`,
  `admission::ordered`, `sdk_owner::due_unowned`, `provider_transition::ready_in`), still around its own slots and
  hosts. Every other step above is still not driven. They are listed as what the model does not cover in the
  module doc of `machine-model/src/lib.rs` and in `task-states.md` ([Rules](task-states.md#rules)), which no longer
  says «no known gaps»: `KNOWN_GAPS` keeps its meaning, rules the code does not apply yet, and stays empty.

### Findings

#### F-LIFE-1 · An unstarted task that waits for other tasks shows as "Queued" (slot wait), not "Waiting for tasks"
**Status: FIXED** in #553, in the lifecycle aggregate as its single source (the user decided on 2026-11-23
that the wait is a state of the task's machine, not something read beside it). A task no execution runs and whose
dependencies have not landed is `AwaitingTasks{tasks, start}`: it owes `(Party::Tasks, Owed::Tasks)` (or the plan
review's `Answer`) and `Admit` refuses it with `AwaitsTasks`, so no node takes it. `record_start_waits`
(`domain-rules/src/lifecycle.rs`), at the end of every command, records `WaitsForTasks{tasks}` when the set of tasks it
may not start before (`unmet_dependency_ids`) differs from the one its lifecycle waits for, and `TasksLanded` when that
set is empty: the task enters the wait in the command that creates it, follows `SetDependencies`/`SetRelations`, leaves
it in the command that lands its last dependency, enters it again when a dependency is reopened, and ends it with
`Finish` when cancelled. A reopened task waits the same way from `Queued` and starts again at its stage.
`AssignTask` on a waiting task, and the orchestrator's `launch` of one, are refused with `dependency_not_done`. Tasks stored waiting before this record existed
load as they were kept; `ReviewWaits`, the first command the daemon sends on start, records their wait. The read beside
the lifecycle in `obligation_with` is gone. Tests:
`lifecycle_tests::an_unstarted_task_records_its_wait_for_another_until_it_lands`,
`situation_tests::an_unstarted_task_that_waits_for_another_waits_for_tasks_not_for_a_slot` (without the fix:
`Waiting(Slot)`), `store-sqlite` `a_stored_wait_from_before_the_lifecycle_recorded_it_is_recorded_on_the_first_command`,
and in `crates/domain/tests/lifecycle.rs` the three tests of `AwaitingTasks` plus the property tests that now
generate `DependenciesLanded`.
**Class**: CONFIRMED · **Severity**: S2 (wrong visible state and counters; nothing is lost) · **Probability**: high (every task created with `dependencies`, e.g. stacked phases, goes through this while its dependency has not landed) · **Overlap**: no task in the plans of related work and no open issue covers it (#323 is about the outcome of dependents when one is cancelled, not their visible state). `git log` of `situation.rs`: no related changes since #297. The evidence is new.
**Status**: still holds at `3fc0370ea`. #536 only changes which tasks the Tasks list shows (every task that has not ended is in the active list, M-CARDS-13); the block each one goes to still comes from `Tally` of its `Situation`, which #536 does not touch. Refreshed refs at `3fc0370ea`: `LifecycleRecord` `domain-rules/src/lifecycle.rs:295-496`, `event()` `:617-803`, `plan_lifecycle` `:1277-1284`, `state_within_phase` `:2625-2626`, `waits_are_over` `rules.rs:1986-2002`, blocks `views/src/tasks.rs:545-555`.
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
**Status**: still holds at `3fc0370ea`. #536's `reopen_requests_with_open_tasks` does not reach it: `ReopenTask` still requires the request to be open (`reopen_guard`, `guards.rs:432-438`), so the request is never closed at that point. Refreshed refs at `3fc0370ea`: `ReopenTask` `rules.rs:5308-5328` (kept node and synthetic execution `:5324-5327`), host stamp `claude-bg` `domain-rules/src/lifecycle.rs:624-629`, `current_sdk` `:2724-2732`.
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
- **Status: fixed** in #552 (commit `56d796f26`) with the default decision: `ReopenTask` no longer keeps the node (every live node runs through the SDK, `NodePhase` has only `ThroughSdk` and `Closed`), so a reopened task waits with no node in the launch queue and the gate's start resumes the saved session. Tests: `sdk_node_tests::a_task_reopened_with_its_sdk_node_kept_by_the_user_waits_for_a_new_execution_that_is_heard` (replaces `reopening_with_the_original_node_kept_cannot_change_its_native_session_account`, which asserted the old behaviour) and `tests::a_task_reopened_with_its_node_kept_by_the_user_waits_for_a_new_node` (replaces `…_keeps_working_with_it`). Ablation: restoring the kept node makes both fail on the node assert.

### Hypotheses

#### H-LIFE-1 · A repeated `TurnStarted` after its `TurnEnded` reopens the turn and its end is swallowed: "Working" with no turn
What is claimed: the domain journal stores neither provider cursor nor stamp (`event()` always uses "current cursor + 1" and the current stamp, `domain-rules/src/lifecycle.rs:617-620`), so the aggregate cannot refuse a repeated fact. A `TurnStarted{X}` with the activity in `Resting/Awaiting` opens a new turn (`observations.rs:225-227`, `provider_turn_started`). The next `TurnEnded{X}` is ignored because `turn_end_recorded` already sees an end of X since the last `Admitted` (`lifecycle.rs:1560-1564`, `:1590-1592`). Result: `Activity::Turn` with no end → `Owed::Turn`, `Situation::Working(Turn)`, no deadline, until the host dies.
Missing to confirm: that a restart can re-deliver already-applied facts to the domain, that is, whether the `RecordRuntimeFacts` commit and the advance of the normalization checkpoint (`provider_normalization_checkpoints`) are atomic (STORE/ENGINE). The model (`machine-model/src/lib.rs:423-447`) deduplicates with its own cursor, which `restart` does not clear (`:383-395`), so it does not exercise this. Proposed test: a scenario with host doubles that re-emit the batch after a simulated restart between commit and checkpoint; or a domain test applying `TurnStarted{X}`, `TurnEnded{X}`, `TurnStarted{X}` and checking the task does not read `Working`. Overlap: scope of the stalled-node recovery work (phase 2, `StreamEndedMidTurn`/contradictions): new evidence for them, not a fix of its own.

#### H-LIFE-2 · `finish_settled_deliveries` can refuse the command that settles a delivery with an open orchestrator case
What is claimed: it runs in the post-processing of **every** command (`rules.rs:2951`; `:2962` at `3fc0370ea`, unchanged by #536, which adds `reopen_requests_with_open_tasks` later in the same sequence) and its error aborts the command. Its filter (`lifecycle.rs:2569-2578`) looks at `DomainState` predicates (`unanswered` excludes system cards and `blockers` excludes the orchestrator's, `rules.rs:2301-2355`), but the aggregate's `finish()` refuses `Complete` if any card other than `Recovery` is left (`commands.rs:1045-1053`). An orchestrator `Unheard` card (stopped-node case, created by `Unfinished` in `Delivered`, `commands.rs:899-923`) passes the filter and makes `finish` fail.
Missing to confirm: a real sequence in which the delivery is settled (accepted or merged) with that case open and no node (`!wraps_up`): e.g. open case → the session dies (`RecordNodeEnd{Gone}`, which releases the execution and keeps the `Delivered` cards, `commands.rs:1187-1192`) → the user accepts. If reached, `AcceptDelivery` (or the merge observation, or a command of another task that makes this one "completable") is refused with `lifecycle refused` until the orchestrator settles the case. Proposed test: a domain test with that sequence; assert the command that settles the delivery is accepted and the task ends or the case is withdrawn. Overlap: ORCH (`Answer` deadline) and phase 4 of the stalled-node recovery work.

#### H-LIFE-3 · The node list (`/runtime`) says "Busy" while the task says it waits
`from_lifecycle` (`daemon-engine/src/live.rs:403-424`) translates every `LiveState::Working(_)`, including `Working(Alondra(Answering|Starting|Delivering|Waking|Merging|Finishing))`, into `NodeState::Working`/`Busy`, while `Situation` reads it as waiting (`situation.rs:239-247`) and the doc says an Alondra step "is not work". Missing: confirm which screen shows the `/runtime` node state today (the Fleet screen is being retired). S3.

#### H-LIFE-4 · The task of F-LIFE-2 does not leave "Delivering" by itself
Missing: confirm in the engine that no loop picks up a `claude-bg` execution in `InputPending` of a `ThroughSdk` node (gate: `passes_gate_in` requires `admitted_through_sdk`; `obligation_watch`: `Requeue` only if `hosts().owns(agent)`; `bg_handover` only advances handovers already recorded, `bg_handover.rs:235-242`).
**Status: CONFIRMED, then fixed with F-LIFE-2** (#552). `engine-sdk` `auto_launch::tests::a_task_reopened_with_its_node_kept_by_the_user_gets_a_new_execution_that_resumes_its_session` runs the real automatic-launch pass and the gate twice after the reopen. With the `ReopenTask` fix reverted, no loop starts anything: after both passes the task still has the kept node and its `claude-bg` execution, and the test fails on that host (`left: "claude-bg"`). With the fix, the gate starts a new SDK execution (generation 2) that resumes the original session.

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

**Status: fixed** in #624 (group G12), each checked again at `b479c4f7c` before the change, where all eight still held:
1. `task-states.md` places `Situation` in `crates/domain-rules/src/situation.rs`, computed in the `ReadIndex` from the
   lifecycle, the row and the listed cards; icons, detail lines and header counts read it (`classify`,
   `activity_counts`), so the old note that some views still read the old state is gone.
2. The obligation table has `Replan`, and says that `Situation` and the clock read `obligation_of`, which adds
   `Answer` (a delivery sent back while its node waits on the orchestrator) and `Replan` to the lifecycle's.
3. Rule 3 now says the generation, the store's cursor and `turn_end_recorded` turn down facts about another
   execution or already recorded, and names the facts the aggregate refuses and that one refusal refuses the batch.
4. Rule 4 names the two callers that still ask the aggregate before choosing an event (`end_of_pause`,
   `record_host_ended`). Making them single-event is not part of G12.
5. The precedence lists the closed node, the closing delivery, the unlisted card and the cancellation winding down.
6. The model test lists what it does not drive (M-LIFE-14) and no longer claims «no known gaps».
7. The deadlines section lists the two deadlines of the policy and every engine timer outside it (F-ENGINE-5).
8. «Where the result differs from the plan» lists the unreachable branches. Re-checked: `ResourceReleased` is now
   emitted by the Codex normalizer, and `WaitForDependencies`/`DependenciesLanded` are recorded since #553 but only for
   a task that has not started, so `Progress::Dependency` (a started task that waits) is still unreachable, as are
   `PlanApproved`, `BlockDelivery`, `DeliveryUnblocked`, `MergeObserved`, `DependenciesResolved`,
   `RetireReleasedExecution` and `Event::Ask`. Removing them is not part of G12.

### M-LIFE-15 · Task stages (`TaskStages`, added by the stages phase)

```toml state-machine
id = "M-LIFE-15"
code = ["crates/domain-rules/src/stages.rs::TaskStages", "crates/domain-rules/src/stage_rules.rs::begin_stage_handover", "crates/domain-rules/src/stage_rules.rs::record_plan_approval", "crates/engine-sdk/src/provider_transition.rs::request_stage"]
truth = ["sqlite:task_stages"]
```

- **Kind**: explicit, product. A task is no longer one agent: a task the orchestrator creates with
  `stages: {staged: true}` is worked by successive executions, each a new agent instance with the
  role (`ExecutionRole ∈ {Author, Implementer, Validator}`), model and account of its stage, one at
  a time, in the same worktree and PR. A task without stages keeps one execution with no role, as
  before.
- **Components**: `staged` × `reason` (the orchestrator's choice, shown on the task) × `selections`
  (the selection each role starts with, kept apart from `Task.selection`: what the user chose by
  hand holds in every role, the role's profile fills the rest) × `handover: Option<StageHandover{to,
  cause, transition, attempts, refused}>` × `approvals` (every plan the user approved: card,
  revision, generation, role; at most 32) × `runs` (every execution: generation, agent, role,
  provider, account, model; at most 64).
- **Moves**:

| From | Event [guard] | Result |
|---|---|---|
| no runs | `RecordTaskStages` [system, before the first node starts, a selection for every role iff staged] | `staged`, `reason`, `selections` set |
| any | `AdmitExecution` | a run with `next_role()` (the handover's role, else the current one, else `Author` for a staged task); a handover to that role ends |
| role `Author`, no handover | the user approves a plan (`AnswerQuestion` with `approve` on a `Hold::Plan` card) | approval recorded; `handover = Implementer`; the input the implementer starts from is queued (`Told`), so the author's turn ends on it; the author's decision is `HandedOver` (Claude is denied with `HANDED_OVER_PLAN`, Codex settles without a new turn) |
| any other role, or no stages | the same approval | approval recorded only; the decision is `Allow` |
| handover due (`transition = None`, not `refused`) | `BeginStageHandover` [system, stamp matches, no pending selection, the role's selection allows the target] | `selection` = the role's, `pending_selection` = target, `transition` set |
| handover with `transition` | `StageHandoverFailed` naming it | `transition = None`, `attempts + 1`; `refused` once `attempts = 3`, or at once when the start is refused for good (`REFUSED_STARTS`) |
| handover due, the task has no execution, no deferred or refused move and no open start problem | `StageHandoverFailed` with no `transition` and `refused`, with its `StageStartRefused` start problem | `attempts + 1`, `refused` |
| handover `refused` | the user answers `retry` on its `StageStartRefused` start problem | `attempts = 0`, `refused = false` |

- **Engine**: `provider_transition::pass` starts every due handover (`request_stage`: account and
  model through `select_account` with the role's selection) as a `Kind::Stage` selection change
  (M-ENGINE-7). While the task owes a role its current execution does not play
  (`owes_role_change`), no input reaches that execution and nothing relaunches or retakes it.
- **Roles and tools**: `ExecutionRole::allows(RoleTool)`. The author plans and never registers,
  presents or declares work after a PR; the implementer does all but `record_verdict`; the
  validator never plans, registers a PR or records a verdict. A refused call is
  `role_forbids_tool` with the tool as subject and what the role does instead; a plan its role may
  not send, or the author's plan after its approval, is denied by policy without a card.
- **Persistence and restart**: the whole record is one JSON row of `task_stages`, written by the
  commit that changes it and read with the task; a task without a row has no stages. The handover
  survives a restart: its selection change resumes from its journal (M-ENGINE-7), or, aborted, the
  next pass starts it again.

## Cards, plans, documents and requests (`CARDS`)

Scope: the interaction machines and the work machines of the domain, outside the turn/node lifecycle. Code: `crates/domain-rules/src/` (mainly `rules.rs`, `guards.rs`, `model.rs`, `card_contract.rs`, `rules/plan_review.rs`, `rules/snooze.rs`, `rules/open_pr.rs`, `ownership.rs`, `projection.rs`, `attention.rs`), `crates/engine-orchestrator/src/orchestrator_inbox.rs` (only where it closes requests), `crates/store-sqlite/src/lib.rs` (schema) and `crates/store-sqlite/src/command.rs` (effect outbox). Audited commit: `1567967b9`.

Execution model common to all machines in this area (stated once):

- **Source of truth**: `DomainState` (`model.rs:1416-1508`) persisted in `domain.db` (tables `interactions`, `requests`, `tasks`, `task_waits`, `task_checklists`, `delivery_checklists`, `request_plans`, `plan_reviews`, `request_folds`, `task_creations`, `cleared_requests`, `task_post_merge_work`, …; `store-sqlite/src/lib.rs:79-216`). The SQL schema only enforces foreign keys, `json_valid` and some ranges; **no card invariant lives in SQL**: all of them are in `DomainState::validate` (`rules.rs:5886-6260`), which runs after every command and on load.
- **Transition**: `prepare_transition` (`rules.rs:2926-3028`) clones the state, applies the command (`apply`, `rules.rs:3029-5745`) and then runs a fixed chain of sweeps: `end_lapsed_awaits`, `withdraw_released_stalls`, `hand_back_replaced_deliveries`, `withdraw_deliveries_asked_after` (`:2370`), `withdraw_deliveries_behind_their_targets` (`:2395`, PRs and documents since #551), `adopt_declared_post_merge`, `finish_settled_deliveries`, `withdraw_cancelled_requests` (`:2271`), `derive_states`, `cancel_orphan_reviewers`, `retire_cards_of_ended_tasks` (`:2473`), `reconcile_snoozed_cards`, `close_finished_messages` (`:1309`), `close_delivered_plans` (`:1368`), effect `RequestTasksOver` (`:2961-2969`), `NotifyUser` of `acting` (`:2970-2975`), `resume_waiting_nodes` (`:1746`), `ended_task_waits` (`:2977-2986`), `wrap_up_step` (`:2549`), `validate`. Everything is atomic (clone-validate-swap).
- **Concurrency**: a single writer; the store applies each command inside an `IMMEDIATE` transaction (`store-sqlite/src/owner.rs:122`) together with its semantic receipt and its outbox (`domain_messages`, `store-sqlite/src/command.rs:290-400`).
- **Freshness**: `admit` (`guards.rs:77-99`) rejects `stale_revision` if `expected_revision` is lower than the revision of the *subject* (`subject_revision`, `rules.rs:2760-2900`): the task for task commands, the interaction for decisions on a card. Every decision on a card raises its `revision`, so two devices deciding on the same card with the same snapshot are serialized: the second gets `stale_revision`.
- **Idempotency**: same `(actor, command_id, epoch)` → the store returns the first result (see M-STORE-2); receipts are deleted by retention. A user/node/orchestrator command that changes nothing is rejected with `no_effect` (`rules/effect.rs:8-35,86-88`); Alondra's (`Actor::System`) may change nothing without error.
- **Restart**: everything in this area is persisted state; nothing lives in memory except the orchestrator's clocks (M-ORCH-6). Effects (`NotifyUser/Agent/Orchestrator`, `RequestTasksOver`, `ReviewDependentPlans`) are written to the outbox in the same transaction and delivered once (M-STORE-1).

### M-CARDS-1 · Life of a card (`Interaction`)

```toml state-machine
id = "M-CARDS-1"
code = ["crates/domain-rules/src/model.rs::Interaction", "crates/domain-rules/src/rules.rs::open", "crates/domain-rules/src/rules.rs::invalidate", "crates/domain-rules/src/rules.rs::retire"]
truth = ["sqlite:interactions", "sqlite:interaction_attachments", "sqlite:interaction_holds", "sqlite:interaction_reads", "sqlite:interaction_subjects"]
```

- **Kind**: implicit, composite. State = `withdrawn: bool` × payload (`Information.read_by`, `Question/ActionRequest.answer`, `Delivery{acceptance, is_final, feedback}`) × `task.final_presentation` × `holds` × `snoozed_until` × `clarification`. **Defined in**: `model.rs:906-925` (payload), `:1296-1326` (`Interaction`). Opening: `open()` (`rules.rs:1006-1021`).
- **Source of truth**: `DomainState.interactions` (table `interactions`). **Copies and derivatives**: read index (`read_index.rs`, checked equal with `assert_index_matches` by `read_index_tests.rs`), inbox (`projection.rs:381-405` `inbox_pending`/`card_waits_for_user`), `attention` (`attention.rs:140-189`), apps. **Duplicate that diverges**: `open()` and `card_waits_for_user()` are two definitions of "open"; the inbox also requires an active task, not snoozed, not `out_of_sight`, and for deliveries that `attention` be that card. A system card of an ended task is still `open()` but is not listed (see H-CARDS-2).
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
- **Invariants**: enforced (`validate`): identity/revision, agent card with task, coherent subject, answers within the options, valid acceptance only on the current final delivery (`rules.rs:6190`), feedback directed at a target of the delivery. **Enforced since #551**: (a) a non-ended task's `final_presentation` points to a non-withdrawn delivery (`hand_back_replaced_deliveries`, and `presentation()` refuses a withdrawn one) → F-CARDS-5; (b) a question group stays complete or is withdrawn whole (`replaceable` refuses a member) → F-CARDS-2; (c) the targets of the current delivery stay current, documents as PRs (`withdraw_deliveries_behind_their_targets`) → F-CARDS-4. **Assumed, not enforced**: (d) the contract's "Goes away when" (`card_contract.rs:200-214`, column of `docs/architecture/cards.md`) is **documentation only**: `Contract.retires` is only read to generate the table (`card_contract.rs:515`); no rule applies it.
- **Persistence and restart**: everything persisted; the Claude hook waiting on a `HoldHook` lives in the host process (out of scope).

### M-CARDS-2 · Final delivery, acceptance and merge (`acceptance_valid` × `MergeAuthorization`)

```toml state-machine
id = "M-CARDS-2"
code = ["crates/domain-rules/src/model.rs::Acceptance", "crates/domain-rules/src/model.rs::MergeAuthorization", "crates/domain-rules/src/model.rs::MergeAttemptState"]
truth = ["sqlite:merge_authorizations", "sqlite:stack_readiness", "sqlite:delivery_feedback", "sqlite:delivery_targets", "sqlite:lifecycle_delivery", "sqlite:pr_decisions"]
```

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
- **Invariants**: enforced since #551: an approved plan is read-only. Its effects are irreversible (request created and orchestrator notified), and feedback on it no longer invalidates its acceptance: it becomes a request about the build (`feedback_on_approved_plan`) → F-CARDS-3.

### M-CARDS-3 · Question group (`QuestionGroup`)

```toml state-machine
id = "M-CARDS-3"
code = ["crates/domain-rules/src/model.rs::QuestionGroup", "crates/domain-rules/src/rules.rs::group_complete", "crates/core/src/model.rs::QuestionStatus"]
truth = ["sqlite:interactions"]
```

- **Kind**: implicit. **Defined in**: `card.group{id,index,total}` on each member; ids `<group>-q<n>` (`model.rs:1100-1129`, `rules.rs:204-221`). Complete = all non-withdrawn members answered (`group_complete`, `rules.rs:1069-1071`); `group_members` includes the withdrawn ones (`:1030-1037`).
- **Events**: `AskQuestions`/`HoldQuestions` (create N), `AnswerQuestion` (one), `ReopenAnswer` (undoes one while the group is incomplete), `Withdraw` (withdraws all and deletes answers). `replaces` refuses a member of a group (`question_in_group`, since #551); questions hold no slot, so `slot_holders` never retires one → F-CARDS-2.
- **Effect on completion**: `NotifyAgent` with all the answers; `hold_for_plan_review` (`plan_review.rs:76-106`).

### M-CARDS-4 · Snooze (`snoozed_until`, `resurfaced_at_revision`)

```toml state-machine
id = "M-CARDS-4"
code = ["crates/domain-rules/src/rules/snooze.rs::snooze_guard", "crates/domain-rules/src/rules/snooze.rs::resurface_card", "crates/domain-rules/src/rules/snooze.rs::reconcile_snoozed_cards"]
truth = ["sqlite:interactions"]
```

- **Kind**: implicit. **Defined in**: `rules/snooze.rs:4-128`.
- **States and transitions**: visible → snoozed (`SnoozeCard`, guard: open, to the user, in the inbox, task not archived, `until > now`) → visible again by `ResurfaceDueCards{now}` (System), `UnsnoozeCard` (user), new activity of a node on the task, or replacement (`resurfaced_at_revision` on the new card, `rules.rs:808-821`). Cleared on withdrawal (`retire`) or on close; a task that ends → the snoozed card is withdrawn.
- **Clock**: `now` is supplied by the adapter; nothing in the domain compares against a clock. No findings.

### M-CARDS-5 · Clarification (`clarification`, `answer_clarification`)

```toml state-machine
id = "M-CARDS-5"
code = ["crates/domain-rules/src/model.rs::Interaction"]
truth = ["sqlite:interactions"]
```

- **Kind**: implicit. `RequestClarification` (`rules.rs:4213-4246`) sets `clarification`; R7 (`clarification_answered`, `:1086-1108`) forces the author's next non-Know card to replace it (`replaces`); the MCP tool `answer_clarification` fills `replaces` and forwards to `ask`, `request_action`, `request_input`, `request_test` or `present_result` (`cli-mcp/src/mcp.rs:1195-1205`). It is also offered on open final deliveries (`projection.rs:592-611`). A delivery with a clarification holds its merge (`merge_held`, `projection.rs:246-249`). Issue #317 proposes withdrawing it whole. It is the main entry to F-CARDS-5.

### M-CARDS-6 · Slots and replacement (`replaces`, `CardSlot`)

```toml state-machine
id = "M-CARDS-6"
code = ["crates/domain-rules/src/model.rs::CardSlot"]
truth = ["derived"]
states = "crates/domain-rules/src/model.rs::CardSlot"
variants = ["Plan", "FinalResult", "Problem"]
```

- **Kind**: explicit, small. **Defined in**: `CardSlot{Plan, FinalResult, Problem{cause}}` (`model.rs:1078-1098`). In a task the slot belongs to the task whatever the author; outside, to the author (`rules.rs:1149-1160`). `replaces` requires same author, not hook, not answered, open (`:1129-1147`).
- **Copies that diverge**: `replaces` rejects hook cards, `slot_holders` does not (H-CARDS-1); replacing the current delivery through either one now hands it back like `Withdraw` (`invalidate` + `hand_back`, sweep `hand_back_replaced_deliveries`, since #551) → F-CARDS-5.

### M-CARDS-7 · Plan review of dependents (`plan_reviews`)

```toml state-machine
id = "M-CARDS-7"
code = ["crates/domain-rules/src/model.rs::PlanReview", "crates/domain-rules/src/rules/plan_review.rs::plan_review_pending"]
truth = ["sqlite:plan_reviews"]
```

- **Kind**: explicit. **Defined in**: `PlanReview{task_id, decisions, dependents: BTreeMap<TaskId, Option<PlanReviewSettlement>>}` (`model.rs:791-832`), `rules/plan_review.rs`.
- **Events**: opened when the user completes a decision (`Decide`, not permission/plan/elicitation) of a task with unstarted dependents: each dependent gets `HeldForPlanReview` and the orchestrator `ReviewDependentPlans`. Settled per dependent with `SettlePlanReview{Unchanged|PlanChanged|Cancelled}` (orchestrator; System only `Unchanged`, `Answer` deadline of M-ORCH-6) or in bulk with the problem card `keep_plans` (`KeptByUser`).
- **Guards and invariants**: `not_held_for_plan_review` in `AssignTask`; `validate_plan_reviews` (`plan_review.rs:212-227`): no open dependent with a node. A cancelled dependent stays `None` forever but `plan_review_waiting` filters it. No finding of its own.

### M-CARDS-8 · Checklist of the approved plan (`checklists`, `checklist_marks`, `post_merge_work`)

```toml state-machine
id = "M-CARDS-8"
code = ["crates/domain-rules/src/checklist.rs::Checklist", "crates/domain-rules/src/checklist.rs::ChecklistMark", "crates/domain-rules/src/rules.rs::take_plan_items"]
truth = ["sqlite:task_checklists", "sqlite:delivery_checklists"]
```

- **Kind**: implicit. **Defined in**: `checklists: TaskId → Checklist{plan, items}` (tables `task_checklists`, `delivery_checklists`). Origin: approving a plan held by a hook (`AnswerQuestion` with `approve`, `rules.rs:4371-4376`), creating the task of a `request_plans` (`rules.rs:3676-3684`), `plan_items` (`take_plan_items`, `:1838-1863`) or `continues` (`:3664-3672`). Marks per final delivery (`checklist_marks_cover_the_plan`, `:600-662`); proof required to complete, confirm cleanup and finish post-merge work (`require_checklist_proof`, `:668`). `DeclarePostMergeWork` lets a PR be delivered with partial marks (`docs/architecture/cards.md`, "A PR review before publication").
- **Invariants**: enforced (`validate`, `rules.rs:6226-6245`): each checklist belongs to a plan of its request; each item has a single task. Assumed: items moved with `plan_items` to a task that is later cancelled only come back through `continues` (no rule returns them). No confirmed finding.

### M-CARDS-9 · Documents and revisions

```toml state-machine
id = "M-CARDS-9"
code = ["crates/domain-rules/src/model.rs::Document", "crates/domain-rules/src/model.rs::DocumentRevision", "crates/domain-rules/src/model.rs::DocumentReview"]
truth = ["sqlite:documents", "sqlite:document_revisions", "sqlite:document_reviews", "sqlite:document_decisions", "sqlite:task_documents"]
```

- **Kind**: implicit. **Defined in**: `Document{current_revision}`, immutable `DocumentRevision{number}`, `DocumentReview{decisions}` per `(task, document)` (`model.rs:488-515`).
- **Events**: `CreateDocument` (node), `ReviseDocument` (node; invalidates **all** tasks that link the document, `rules.rs:4502-4552`), `LinkTaskDocument` (manager; shares a document between tasks), `DecideDocumentReview` (user; current revision only). A final delivery with `DocumentRevision` is valid only while that revision is the current one (`targets`, `rules.rs:690-712`). Since #551 the sweep `withdraw_deliveries_behind_their_targets` returns such a delivery to its node, with a notice naming the document → F-CARDS-4.

### M-CARDS-10 · Requests (`Request`)

```toml state-machine
id = "M-CARDS-10"
code = ["crates/domain-rules/src/model.rs::Request", "crates/domain-rules/src/request_outcome.rs", "crates/core/src/tasks.rs::RequestState"]
truth = ["sqlite:requests", "sqlite:cleared_requests", "sqlite:maintenance_requests", "sqlite:message_requests", "sqlite:request_card_replies", "sqlite:request_folds", "sqlite:request_launch", "sqlite:request_origins", "sqlite:request_plans"]
```

- **Kind**: implicit. **Defined in**: `Request{closed, withdrawn, answer, message, maintenance, cleared, origin, folded_into, reply_to}` (`model.rs:41-88`) + the request's tasks + `request_plans` + cards `system:request_tasks`/`system:request_done`/`<id>-answer`.
- **States**: being defined (open without tasks, `is_defining`), with live tasks, with all tasks ended and open (only the orchestrator moves it), closed with answer (Done/answer card), closed withdrawn (no card), folded into another task, cleared (`cleared`).
- **Transitions**: `CreateRequest`/`CreateCardReply` (user), `TitleRequest`, `CloseRequest` (`rules.rs:3511`; `close_request` `:1342-1366`: Done card if it has tasks and the user asked for it, answer card if not), `close_delivered_plans` (automatic close only if all tasks are Done and marked every item of every plan, `:1368-1410`), `close_finished_messages` (messages, no card), `withdraw_cancelled_requests` (the user cancelled all unstarted tasks: closed withdrawn, no card), `AdoptOpenPr` (reopens the request, `rules.rs:5272-5277`), `ClearCompleted`. Effect `RequestTasksOver` (`:2961-2969`) **only once**, in the commit in which the last task ends.
- **Orchestrator inbox**: `orchestrator_inbox::steps` and `unresolved` chase requests **without tasks** and, since F-CARDS-1 was fixed, requests whose tasks **all ended**: reminder `tasks-ended-unsettled-<request>-<n>` at each rest of the orchestrator until `TASKS_ENDED_REMINDERS` (2) were read by batches that ended with their result, then `Step::Close` by System with the Done card of `tell_it_is_done`.
- **Re-checked after #536** (at `3fc0370ea`):
  - **New transition**: `reopen_requests_with_open_tasks` (`rules.rs:1307-1318`) runs in the post-processing of
    **every** command (`:2971`, after `forget_what_ended_tasks_allowed` and before `close_finished_messages`). Any
    request that is `closed` or `cleared` while one of its tasks is not `over()` gets `closed = false`,
    `withdrawn = false`, `answer = None`, `cleared = false`. It is the same reset `AdoptOpenPr` already did
    (`:5290-5293`).
  - **Invariant it states**: a request is closed or cleared only while none of its tasks is open, "open" meaning
    `!over()` (`over()` = terminal or `ending.is_some()`, `domain-rules/src/lifecycle.rs:1184-1186`). A task that is
    `Closing` (lifecycle `Finishing`) is over, so a closed or cleared request may still hold one: that is how
    `CloseRequest` (guard `!t.over()`, `rules.rs:3530-3536`) and `RequestTasksOver` (emitted when the last task becomes
    over, `:2974-2981`) already work, and `ClearCompleted` clears such a request on purpose
    (`closed_with_tasks_still_ending`, `guards.rs:517-523`).
  - **Who can make it fire**: only `ReopenTask` and `AdoptOpenPr` turn an over task into an open one (no event leaves
    `Finishing` except to a terminal state: entered at `domain/src/lifecycle/commands.rs:1103`, left only at `:1016`). `ReopenTask` requires an
    open request (`guards.rs:432-438`) and `AdoptOpenPr` resets the request itself, so in a database this release
    wrote the rule changes nothing; it repairs states an earlier release left (a closed or cleared request over an
    open task), on the first command after load.
  - **Persistence**: `closed`, `withdrawn` and `answer` live in the `requests` row and are written by the upsert; at
    `3fc0370ea` the reset of `cleared` was **not** written (the side table only got inserts), so a later full reload
    read the request as cleared and open and refused the whole database → F-STORE-7, fixed since by G3 (#520): the
    save now deletes the `cleared_requests` row.
  - **Done card**: the reset leaves the request's «Done» card where it was; when the request closes again a second
    one is written next to it (H-CARDS-5).
  - Refreshed refs at `3fc0370ea`: `CloseRequest` `rules.rs:3523`; `close_request` `:1354-1376`;
    `close_delivered_plans` `:1380-1426`; `close_finished_messages` `:1321-1326`; `RequestTasksOver` `:2974-2981`;
    `withdraw_cancelled_requests` `:2283-2300`; `AdoptOpenPr` `:5270-5306`; `ClearCompleted` `:5735-5742`; `validate`
    refuses a cleared request that is open at `:5932-5934`.

### M-CARDS-11 · Task creation, dependencies and waits

```toml state-machine
id = "M-CARDS-11"
code = ["crates/domain-rules/src/rules.rs::dependency_graph", "crates/domain-rules/src/rules.rs::blocks_graph"]
truth = ["sqlite:task_dependencies", "sqlite:task_waits", "sqlite:lifecycle_dependencies", "sqlite:task_continuations", "sqlite:task_creations", "sqlite:task_launch_plans", "sqlite:task_relations"]
```

- **Kind**: implicit. **Defined in**: `Task.dependencies` (objective, "landed"), `delivered_dependencies` (reviewer waits for the delivery), `waiting_for_tasks` + `ended_task_waits` (wait declared by a node, ends with any final state), `stack` (`StackDependency`), `blocks` (reviewer). Acyclic graph (`dependency_graph`, `rules.rs:1865-1898`; `blocks_graph` `:1900-1934`). Derived state `WaitingDependency` (`lifecycle.rs:2613-2650` `state_within_phase`).
- **Commands**: `CreateTask` (`rules.rs:3584-3697`, orchestrator only, with `launch` and `priority`, F-ORCH-8 fixed; refuses a cancelled dependency, F-CARDS-6 fixed), `SetDependencies` (`:3771`, refuses a cancelled dependency), `SetRelations` (`:3790`, rejects cancelled tasks), `WaitForTasks` (`:3747`), `DeclarePostMergeWork{waits_for}` (`wait_targets`, `:1778-1820`, rejects cancelled), `SetPriority` (`:3824`).
- **Dependent of a cancelled task**: `cancelled_dependency` (`ownership.rs:320-326`) → `Owed::Replan` only if the task has not started and has no node (`ownership.rs:352-354`); the orchestrator gets a reminder at every rest caused by other work, at most `REPLAN_REMINDERS` (3) (M-ORCH-7, F-ORCH-2 fixed). `waiting_for_tasks` also ends with a cancelled one (`task_wait_finished = terminal`, `rules.rs:2007-2009`) and the node gets `dependencies_done_notice` with `cancelled`. A new dependency on a task already cancelled is refused (F-CARDS-6, fixed); a dependency cancelled afterwards still goes to the orchestrator.
- **Cancellation** (`cancel`, `rules.rs:2191-2235`; `close_task`, `rules/open_pr.rs:29-52`): `invalidate`, withdraws closing/start problems, unanswered questions and cards held by the lifecycle; `Finished{Cancelling}`; `NotifyAgent`. It does not touch dependents (the orchestrator handles them).

### M-CARDS-12 · Stacks (`stack`, `stack_readiness`)

```toml state-machine
id = "M-CARDS-12"
code = ["crates/domain-rules/src/model.rs::StackDependency", "crates/domain-rules/src/model.rs::StackReadiness", "crates/domain-rules/src/rules.rs::stack_ready"]
truth = ["sqlite:stack_readiness", "sqlite:task_stacks"]
```

- **Kind**: implicit. **Defined in**: `StackDependency{upstream_task, presentation_id, pr_id, head}` + `StackReadiness{ready}` published by the upstream node (`PublishStackReadiness`, `rules.rs:5181-5211`); `stack_ready`/`stack_ready_for_merge` block start and merge. `invalidate` turns `ready` off. A cancelled upstream: `cancelled_dependency` treats it as a cancelled dependency. No finding of its own (the identity of moved PRs belongs to the stable pull request identity work).

### M-CARDS-13 · Section of the Tasks list for a request and each task (`Bucket`, derived; added after #536)

```toml state-machine
id = "M-CARDS-13"
code = ["crates/views/src/tasks.rs::Bucket"]
truth = ["derived"]
states = "crates/views/src/tasks.rs::Bucket"
variants = ["Active", "Finished", "Cleared"]
```

- **Kind**: derived, recomputed on every render; nothing stored. **Defined in** (at `3fc0370ea`): `enum Bucket { Active,
  Finished, Cleared }` (`views/src/tasks.rs:115-119`), `bucket` (`:121-131`), `task_bucket` (`:136-144`),
  `bucket_of` (`:146-148`), `request_buckets` (`:150-171`), `Forest::new` (`:204-212`), `listed_active` (`:594-598`),
  header text `request_state` (`:348-366`).
- **Request**: `Active` if any of its tasks is not terminal (`Done`/`Cancelled`); else `Cleared` if `cleared` or every
  task is archived; else `Finished` if `closed` or it has tasks; else `Active`. A request that is a message, or
  answered without tasks, has no bucket until it has tasks. A task whose request does not exist gets one anyway:
  `Active` if any of its tasks is open, else `Finished` (`:165-169`).
- **Task**: `Active` whenever it is not terminal, whatever its request says or whether it has one; otherwise `Cleared`
  if archived or its request is `Cleared`, else `Finished`.
- **Invariant** (`task-states.md`, "Every task that has not ended is in exactly one block of the Tasks list"; test
  `every_open_task_is_in_exactly_one_block_whatever_its_request_says`, `views/src/tests.rs:10664`): an open task is
  never folded or hidden. Before #536 a closed, cleared or missing request put it in Completed or nowhere.
- **Two notions of "open"**: the list uses `!state.terminal()`, the domain (`CloseRequest`, `ClearCompleted`,
  `reopen_requests_with_open_tasks`) uses `!over()`. They differ only for a `Closing` task: closed or cleared
  requests may hold one, and the list now shows that request and task as active until the task ends. Visible effect:
  F-CARDS-7.

### Findings

#### F-CARDS-1 · A request whose tasks ended depends on a single message to the orchestrator: if that batch does not close it, it stays open forever with no result card (issue #353)
**Status: FIXED** in #553, as the user decided on 2026-11-22. `orchestrator_inbox::steps` treats an open request
(not a message) whose tasks all ended like a request without tasks: `tasks_ended` sends the reminder
`tasks-ended-unsettled-<request>-<n>` (new `InboxCard::TasksEndedNotice`, a reserved prefix) when the orchestrator rests
with nothing waiting for it; once `TASKS_ENDED_REMINDERS` (2) reminders were read by batches that ended with their result,
`Step::Close` closes it as System and `close_request` raises the Done card summing up each task. `unresolved` lists such a
request until a reminder is on its way. Test:
`orchestrator_inbox::tests::a_request_whose_tasks_ended_is_reminded_twice_then_closed_by_alondra_with_its_done_card`
(without the fix: no reminder, and `unresolved` is empty).
**Class**: CONFIRMED (that nothing claims it again). Whether the batch fails to close it depends on the model or on a cut, which is probabilistic. · **Severity**: S2 · **Probability**: medium (one omission by the model is enough; issue #353 observed it) · **Overlap**: issue #353 (NEW evidence: exact paths showing why there is no second chance). Related to F-ORCH-1 and F-STORE-3 (ways of losing the single message). `git log` of `orchestrator_inbox.rs`: no later fix.
**Status**: still holds at `3fc0370ea`. #536 adds `reopen_requests_with_open_tasks` in the same post-processing (`rules.rs:2971`), but it only reopens requests that hold an open task; the request of this finding has every task over, so nothing changes for it. Refreshed refs at `3fc0370ea`: `RequestTasksOver` `rules.rs:2974-2981`, `close_delivered_plans` `:1380-1426`, `close_finished_messages` `:1321-1326`.
Still holds at `68770a298`: #516 rewrote the orchestrator inbox, but `steps` and `unresolved` still skip every request that has a task (`orchestrator_inbox.rs:70-73`, `:152-153`), so a request whose tasks all ended is still neither reminded nor raised. #516 only makes the orchestrator's close of it carry the outcome `tasks_ended` (`check_close_outcome`, `domain-rules/src/rules.rs:1357-1387`).
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
**Status**: FIXED in #551 (commit `60d638a0e`): `replaceable` refuses a member of a group with `question_in_group` (field `card.replaces`); the node withdraws the group and asks it again whole. Test: `card_protocol_tests::replacing_one_question_of_a_group_is_refused_and_the_group_is_replaced_whole`, failing on `10ebafced` and passing with the fix.
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
**Status**: FIXED in #551 (commit `60d638a0e`): feedback on an approved plan keeps it approved and its node cleaning up; it becomes a request with `reply_to` naming the plan and its build: a new request for changes, a message to the orchestrator for a reply or a question. Test: `card_protocol_tests::requesting_changes_on_an_approved_plan_becomes_a_new_request`, `replying_on_an_approved_plan_reaches_the_orchestrator_as_a_note_on_the_build`, failing on `10ebafced` and passing with the fix.
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
**Status**: FIXED in #551 (commit `60d638a0e`): the sweep `withdraw_deliveries_behind_their_targets` (the PR sweep extended to documents) withdraws the delivery, hands it back and tells its node which document changed. Test: `card_protocol_tests::a_new_revision_of_a_shared_document_hands_back_the_other_tasks_delivery_and_tells_its_node`, `a_node_that_revises_its_delivered_document_gets_its_delivery_back`, failing on `10ebafced` and passing with the fix.
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
**Status**: FIXED in #551 (commit `60d638a0e`): the sweep `hand_back_replaced_deliveries` gives a replaced current delivery the effects of `Withdraw`, and `presentation()` refuses a withdrawn delivery. Test: `card_protocol_tests::a_progress_note_that_replaces_the_final_delivery_hands_it_back`, `a_question_to_the_orchestrator_that_replaces_the_final_delivery_hands_it_back`, `a_withdrawn_delivery_cannot_be_accepted`, failing on `10ebafced` and passing with the fix.
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
**Status: FIXED** in #553 (the guard; the re-pointing replacement command of #323 is not part of it). Both
commands call `no_cancelled_dependency` before changing anything and refuse with `invalid_input`, field `dependencies`,
like `SetRelations`. Test: `attention_tests::a_task_cannot_be_made_to_wait_for_a_cancelled_one` (without the fix both
commands are accepted).
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

#### F-CARDS-7 · Clear completed takes a closed request whose last task is still ending, which the Tasks list now shows as active (#536)
**Class**: CONFIRMED (at `3fc0370ea`) · **Severity**: S3 (nothing is lost; a request and its task vanish from the list without ever being shown as completed) · **Probability**: medium (the orchestrator closes a request as soon as its last task starts ending, and the node's cleanup takes minutes; any «Clear completed» in that window, offered whenever the Completed section has anything else) · **Overlap**: none in the referred tasks or the open issues. Introduced by #536, which moved such a request from Completed to the active list without changing what the clear takes.
- **Invalid state/transition and broken invariant**: a request with `cleared = true` listed in the active part of the Tasks list, and a «Clear completed» that takes something the Completed section does not show. `clear_guard` says it takes "what the user saw" as completed (`guards.rs:495-497`); since #536 the list shows a closed request with a `Closing` task as active (M-CARDS-13).
- **Evidence** (at `3fc0370ea`):
  1. The last task T of request R starts ending (`Finishing`, row `Closing`, `domain-rules/src/lifecycle.rs:1328`; `over()` is true, `:1184-1186`). `RequestTasksOver` goes out in that commit (`rules.rs:2974-2981`) and the orchestrator's `CloseRequest` passes, since its guard only refuses tasks that are not over (`:3530-3536`). R is closed while T still ends.
  2. `clear_guard` takes R: `closed_with_tasks_still_ending` (`guards.rs:517-523`) is true for a closed, uncleared, non-message request whose tasks are all over and one of them not terminal; `ClearCompleted` sets `cleared` (`rules.rs:5735-5742`). The action is offered on the Completed section whenever it lists anything (`views/src/tasks.rs:744-746`, `projection.rs:125-126`).
  3. The list: `bucket(R)` is `Active` because T is not terminal (`views/src/tasks.rs:121-123`), and `task_bucket(T)` is `Active` (`:136-138`); R's header reads «Active» (`request_state`, `:348-366`). Before the clear R was already in the active list, not in Completed.
  4. When T reaches `Done`, `bucket(R)` becomes `Cleared` (`:124-125`) and `task_bucket(T)` too (`:139-140`): R and T leave every section without ever appearing in Completed.
- **Minimal sequence**: 1) requests R (task T) and R2 (task T2, `Done`, so Completed has an entry); 2) T is accepted and starts its cleanup (`Finishing`); 3) the orchestrator closes R; 4) the user presses «Clear completed»: R2 and R are cleared; 5) the list still shows R and T as active; 6) T ends: R and T disappear.
- **Impact**: the user cannot find the outcome of R in Completed; the clear acted on a row they saw as active.
- **Deterministic test and ablation**: a pure domain test in `crates/domain-rules/src/tests.rs` (fixture with R closed and T `Closing`, plus a `Done` task in another request): `ClearCompleted { seen }` must leave R uncleared. Today R is cleared. A views test in `crates/views/src/tests.rs` next to `every_open_task_is_in_exactly_one_block_whatever_its_request_says` (`:10664`): after T is `Done`, R is in the Completed section. No clock. Ablation: restore `|| self.closed_with_tasks_still_ending(r)` and see both fail.
- **Expected fix and files**: drop `closed_with_tasks_still_ending` from `clear_guard`: the request reaches Completed when its last task ends and the next clear takes it, which is what the list shows since #536. It also removes the usual way a request becomes cleared while it still holds a task (one entry of F-STORE-7). Files: `crates/domain-rules/src/guards.rs`, tests.

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
**Re-checked after #536** (at `3fc0370ea`): still a hypothesis, with a second writer of the same reset. `reopen_requests_with_open_tasks` (`rules.rs:1307-1318`, run after every command at `:2971`) clears `closed` and `answer` of any closed request that holds an open task, and also leaves the Done card (nothing withdraws a `RequestDone` card when its request reopens; `tell_it_is_done` only skips when the orchestrator itself wrote an information card, `orchestrator_told` `:1448-1455`). The paths that reach it in a database this release wrote are still only `AdoptOpenPr` (`:5290-5293`); `ReopenTask` needs an open request (M-CARDS-10). Refreshed refs: `write_system_card` `:1584-1589`. Missing, as before: how the inbox and the apps show two Done cards of one request.

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

```toml state-machine
id = "M-ENGINE-1"
code = ["crates/engine-admission/src/admission.rs::Inner", "crates/engine-admission/src/admission.rs::Saved", "crates/engine-ports/src/admission.rs::Pauses"]
truth = ["memory", "file:admission.json", "sqlite:launch_queue", "sqlite:auto_launches", "sqlite:start_retries", "sqlite:launch_refusals", "sqlite:task_priorities"]
```

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
| Admitted | [`now-at ≥ IN_FLIGHT` and, for a start, its launch no longer under way] | gone | `holds_slot` (F-ENGINE-1, fixed in `e7b898c30`) |
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

```toml state-machine
id = "M-ENGINE-2"
code = ["crates/engine-ports/src/admission.rs::Reason", "crates/engine-ports/src/admission.rs::Cleanup", "crates/engine-ports/src/admission.rs::Paused", "crates/engine-admission/src/admission.rs::hand_pauses"]
truth = ["derived"]
```

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

```toml state-machine
id = "M-ENGINE-3"
code = ["crates/engine-admission/src/admission.rs::hold_queued", "crates/engine-admission/src/admission.rs::release_queued"]
truth = ["sqlite:launch_queue", "sqlite:task_queue_pauses", "file:admission.json"]
```

- **Kind**: implicit. **Defined in**: `hold_queued` (`admission.rs:1224-1256`), `release_queued` (`:1267-1278`)
  and `Pauses.held`.
- **States**:
  - `Queued`: a `launch_queue` row in SQLite, or `asking` with a plan;
  - `Held`: `held[task]` in `admission.json`, no row;
  - `Back`: the row again.
- **Source of truth**: two stores with no common transaction (F-ENGINE-3).
- **Terminals**: none. Nothing deletes `held` when the task ends.
- **Since `e7b898c30`**: one domain fact, `Task::paused_in_queue`, written by `PauseQueuedTask` and
  `ResumeQueuedTask`; the launch keeps its `launch_queue` row, `queued_launches` and `still_waits`
  skip a paused task, and closing the task clears the fact. `Pauses.held` is only read from an
  older daemon's file and handed to the domain once.

### M-ENGINE-4 · SDK host ownership per task (composite, implicit)

```toml state-machine
id = "M-ENGINE-4"
code = ["crates/engine-sdk/src/sdk_owner.rs::Owners", "crates/engine-sdk/src/sdk_owner.rs::Driver", "crates/engine-sdk/src/sdk_owner.rs::take_over_unowned"]
truth = ["memory", "sqlite:provider_execution_owners"]
```

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

- **Lost for want of a sign-in** (Antigravity): when the host's last batch carries the `authentication_required`
  code of `alondra/accountBlocked`, or of `alondra/inputRefused` before `agy` took the input
  (`provider-antigravity/src/context.rs::signed_out`), the `Driver` keeps `native_signed_out` for that host only
  (memory, recomputed from the host's next batch) and `lost_on` sends `RecordHostLost { signed_out: true }`. The rule
  (`domain-rules/src/lifecycle.rs::record_host_lost`) marks the task's account signed out, so no new work starts on
  it, and words the host-lost card as a sign-in problem (`SystemCard::HostLost { signed_out: true }`); its way out
  is the same retake. A stored command or card without the field reads `false`.

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
- **Re-checked after #505** (at `3fc0370ea`; refs: `Driver` `sdk_owner.rs:222-297`, `Owner` `:2578-2598`, `Owners`
  `:2843-2877`, `Driver::new` `:1767`, `add_sdk_owner` `:3007-3016`, `take_sdk_owner` `:3074-3079`): each `Owner`
  now shares one `Handoffs` gate with the driver of every host it runs (`Handoffs`, `:300-354`; field `:294`, copied
  into the `Owner` at `:2618`, carried to the next driver by `replace_ended_host`, `:2438-2442`). The gate is per
  `Owner`, not per agent: two live `Owner`s of the same agent (F-ENGINE-12, branch 2) have two gates.
- **Superseded owner** (26 November, edit_bench under load): an `Owner` whose node is no longer the one its task runs
  on (the task was cancelled, kept by the user and reopened, so the task has none or a new one) ends instead of
  replacing its host or recording a loss (`Driver::superseded`, checked in the `Owner` loop before
  `host_to_replace`). `adopt` and `RecordHostLost` act on the task's current node, so a late old owner used to
  adopt the new execution's node and revoke its owner (`StaleOwner`), leaving a «host lost» card on the new execution.

### M-ENGINE-5 · `Owner` thread loop (explicit in code)

```toml state-machine
id = "M-ENGINE-5"
code = ["crates/engine-sdk/src/sdk_owner.rs::Owner", "crates/engine-sdk/src/sdk_owner.rs::Driver"]
truth = ["memory"]
```

- **Kind**: explicit. **Defined in**: `sdk_owner.rs:2535-2668`.
- **States**: `Stepping`, `Releasing` (`release`/`releases`), `Replacing` (`replacement`, `relaunches`,
  `silent_hosts` with `RELAUNCH_WAITS` waits of 2 s to 300 s), `Lost` (with `lost()`), `Stopped`/`Crashed`.
- **Terminals**: `ended=true` or the thread finished. Nothing reopens them: a new `Owner` is another instance.
- **Deadlines outside the policy**:
  - `opening_deadline` = `io_timeout_seconds` (120 s by default, `sdk_backend.rs:145`): after that time a host
    that did not open its session is replaced (`:1967-1976`, `:2219`);
  - `RELAUNCH_WAITS` and `SILENT_HOSTS` = 3, which ends in `lost()`.
- **Opening deadline since #541** (at `68770a298`): the deadline is `OPENING_TIMEOUT` = 120 s, a constant of its own
  (`sdk_owner.rs:53`; `Driver::new` no longer takes `io_timeout_seconds`, `:1769-1771`). It is armed when an input is
  offered to an unopened session (`:1938`) or on adopting one with a pending input (`:1055`), cleared once the
  session opened (`opening_overdue`, `:2046-2054`). Past it, `step_once` first reads what the host already wrote
  (`readable(Duration::ZERO)` → `observe`, `:2298-2303`) and only gives the host up when nothing is waiting
  (`:2304-2305`). So a host whose session opened just before the deadline, with the frame not yet read, is no longer
  replaced. Each look reads one batch and checks again, so a host that kept writing frames that do not open the
  session would postpone its replacement while it writes; the host emits no periodic frame
  (`agent-host/persistent-main.ts:27-33` only watches its lock), so that stays bounded by what the native process
  writes. A read error counts as nothing waiting (`unwrap_or(false)`).
- **Handoff gate (since #505, at `3fc0370ea`)**: `Owner::spawn` `sdk_owner.rs:2601-2747` (loop `:2619-2744`),
  `Owner::end` `:2808-2824`.
  - **State**: `HandoffState { stopping, handing, stop_waits }` under a mutex with a condvar (`:305-317`).
    `stopping` only goes from false to true and never back: an `Owner` that started stopping hands nothing more.
  - **Handing**: `dispatch` (`:1882-1951`) and `control` (`:1820-1878`) call `begin()` first (`:1883-1887`,
    `:1821-1825`). With `stopping` set they put the permit back at the front of the queue, claim nothing and return
    `Ok`; the loop then sees `told()` (set before the gate is closed) and leaves. Otherwise `handing = true` until the
    `Handing` guard drops at the end of the call, a panic's unwinding included (`:319-326`). The whole claim →
    record → `submit` → receipt sequence, and `session_refresh::refresh`, run inside it.
  - **Stopping**: `Owner::end` sets `stopped`, marks the stop as Alondra's, wakes the seals, then `Handoffs::stop`
    (`:340-349`) sets `stopping` and waits while `handing`; only after that does it ask the host to end (`request` or
    `request_after_interrupt`, `:2813-2818`) and join the thread. Every external stop goes through it: `stop_sdk_host`,
    `close_sdk_host`, `halt_sdk_host` (`:3024-3034`), `stop_sdk_owners` (`:2949-2959`) and `Drop for Owner`
    (`:2827-2833`). `crash()` does not (it stops nothing on purpose, `:2761-2768`).
  - **Bound of the wait**: one handoff, that is the provider's write and receipt deadline (`io_timeout`, at most 300 s,
    `provider-claude/src/adapter.rs:627-628`, `:672-676`), plus the store writes and the refresh file I/O inside the
    gate. No caller holds the store or `Owners.held` while it waits (the list is released before stopping,
    `take_sdk_owner`; the closer and `close_hosts_of_ended_nodes` call it from a blocking task or a thread of their
    own, `task_closer.rs:526`, `sdk_owner.rs:1369-1373`), and the old `join` already waited for everything the
    thread does, so the gate adds no lock cycle.
  - **After an interrupting stop (`close`, `halt`)**: once its loop ended, the owner thread reads and commits what the
    host still says (`Driver::settle_interrupt`) until the host closes, so the turn's interrupted result is recorded
    however long the host takes to write it; it used to drop the connection after one `WAKE`. A host still open
    `INTERRUPT_GRACE` (10 s) after the request, by the clock or by polls that found it silent, is killed and has no
    result to record; `driver.stop()` then waits for its exit as before. A plain `stop` reads nothing more.
  - **Not gated**: the owner thread's own stops (`driver.stop()` at the end of the loop, `:2732`, and inside
    `replace_ended_host`, `:2437`), which run on the same thread after the handoff ended; and a second live `Owner` of
    the same agent (F-ENGINE-12).

### M-ENGINE-6 · Account move (explicit, disabled)

```toml state-machine
id = "M-ENGINE-6"
code = ["crates/engine-ports/src/journal/account_move.rs::Step", "crates/engine-sdk/src/account_move.rs::advance", "crates/engine-sdk/src/account_move.rs::finish", "crates/engine-sdk/src/account_move.rs::undo"]
truth = ["sqlite:account_move_steps", "sqlite:task_move_refusals"]
states = "crates/engine-ports/src/journal/account_move.rs::Step"
variants = ["Planned", "HostStopped", "Admitted", "Done", "Undone"]
terminals = ["Done", "Undone"]
```

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

```toml state-machine
id = "M-ENGINE-7"
code = ["crates/engine-sdk/src/provider_transition.rs::advance", "crates/engine-sdk/src/provider_transition.rs::pass", "crates/engine-sdk/src/provider_transition.rs::Transitions", "crates/engine-ports/src/journal/provider_transition.rs::Phase"]
truth = ["sqlite:provider_transition_steps", "memory", "sqlite:task_models", "sqlite:task_providers", "sqlite:task_selections"]
states = "crates/engine-ports/src/journal/provider_transition.rs::Phase"
variants = ["Requested", "Draining", "SourceFenced", "ContextReady", "TargetAdmitted", "TargetReady", "Consumed", "Done", "Aborted", "Uncertain", "StartRefused"]
```

- **Kind**: explicit. **Defined in**: `Phase` validated in the store
  (`store-sqlite/src/provider_transition.rs:50-60`) and `advance` (`provider_transition.rs:418-505`).
- **States**: `Requested → Draining → SourceFenced → ContextReady → TargetAdmitted → TargetReady → Consumed →
  Done`. Also `Aborted`, `StartRefused` and `Uncertain`.
- **Source of truth**: a SQLite table. The fences (`EndedHost` with its lock held) live in memory
  (`Transitions.fences`) and are rebuilt with `ensure_fence`.
- **Rules**: any `Err` from `advance` leads to `abort()` in `pass` (`:521-526`). `Kind::Model` in transfer phases
  is always rejected: transfer is disabled.
- **`Kind::Stage`** (M-LIFE-15): the change of role of a staged task. It may change account,
  provider and model. At `Draining` it waits until the source rests with nothing pending
  (`stage_drained`: no turn, no own work, no card it waits on, no doubt, no input sent to it) and
  only then ends it; its context is the packet `launches::stage_packet` writes for the role, never
  the conversation; at `ContextReady` it starts its target only once the gate gives the task's
  next turn a slot (`may_take_turn`: slots and memory), else it waits there and looks again on the
  next pass. Its abort records `StageHandoverFailed` and, after the third or at once when the
  start is refused for good (`REFUSED_STARTS`), raises `StageStartRefused` for the user.
- **Concurrency**: the `advancing` mutex serializes its own steps. It does **not** exclude retakes that were
  already running (F-ENGINE-12).

### M-ENGINE-8 · Obligation clock (implicit, in memory)

```toml state-machine
id = "M-ENGINE-8"
code = ["crates/daemon-engine/src/obligation_watch.rs::Seen", "crates/daemon-engine/src/obligation_watch.rs::Action", "crates/daemon-engine/src/obligation_watch.rs::Overdue"]
truth = ["memory"]
```

- **Kind**: implicit. **Defined in**: `obligation_watch.rs:46-56` (`Seen`), with `step`, `observe` and `enforce`.
- **States per task**: `(party, owed, since, first_seen, clock, reported)`. The policy lives in
  `domain-rules/src/obligation_policy.rs`.
- **Persistence and restart**: everything is lost. Deadlines count again from the first observation, which is
  safe: it only delays.
- `effective()` (`:96-101`) turns `HandInput`/`HandDecision` into `Room(Gate)` when the gate says
  `waits_for_slot`.
- **Since #516** (at `68770a298`; `Seen` `daemon-engine/src/obligation_watch.rs:45`, `Action` `:57-62`, `observe`
  `:115`, `enforce` `:148`, `pass` `:206-220`, `apply` `:223-242`): the clock acts only on `HandInput` (`Requeue`) and
  `Wake`; `Action::Answer`, `settle_answers`, the `OrchestratorAnswers` port and the usage-hold aging
  (`World::orchestrator_held`, `last_pass`) are gone. `Owed::Answer` still shows in the per-task state and in the
  log line of overdue steps, but nothing acts on it.

### M-ENGINE-9 · Global usage pause (implicit, in memory, legacy)

```toml state-machine
id = "M-ENGINE-9"
code = ["crates/daemon-engine/src/usage_pause.rs::Record"]
truth = ["memory"]
```

- **Kind**: implicit. **Defined in**: `usage_pause.rs:41-53` (`Record`).
- **States**: `exhausted`, `announced`, `resumed_at`, `strikes`, `by_margin` and `settled_cards`.
- **Persistence and restart**: rebuilt with `usage_probe`. The gate's `Reason::Usage` pauses are only created for
  `--bg` nodes (`:428-445`); SDK nodes wait on `ClaudeHeld` cards of the domain, which belong to another audit (described as M-ENGINE-28 since #540).

### M-ENGINE-10 · Closing a task's SDK node (implicit)

```toml state-machine
id = "M-ENGINE-10"
code = ["crates/engine-ports/src/journal.rs::TaskClosing", "crates/engine-sdk/src/task_closer.rs::due", "crates/engine-sdk/src/task_closer.rs::close_sdk_node"]
truth = ["sqlite:task_closings"]
```

- **Kind**: implicit. **Defined in**: the `TaskClosing` row (`attempts`, `next_attempt`, `rm_started_at`,
  `attempted_cleaned_at`, `since`) plus `due()` (`task_closer.rs:113-123`) and `close_sdk_node` (`:458-530`).
- **Deadlines outside the policy**: `Timing.wait` (cleanup wait), `RETRY` = 5 min and `MAX_FAILURES` = 3, which
  ends in a card.
- **Re-checked after #505** (at `3fc0370ea`; `due()` `task_closer.rs:117`, `close_sdk_node` `:462-533`, `RETRY`
  `:32`, `MAX_FAILURES` `:34`): the states and the row are unchanged. The step "stop the host" (`close_sdk_host`,
  `:526`, through `Owner::close` → `Owner::end`) now waits for the input or control the owner is handing to the host
  to get its receipt before the host is asked to end (M-ENGINE-5, handoff gate). Before #505 the host could close
  without reading the command, the receipt stayed uncertain, `RecordNodeEnd` was refused with `UnresolvedDispatch`
  and the close failed until a retry (counted towards `MAX_FAILURES`). The step can now last up to one handoff
  (`io_timeout`) longer; it runs inside `blocking`, so it holds no store lock. What the gate does not cover: an
  unowned host (`end_unowned_host`, `:517-524`, no handoff in flight by construction) and a second live owner of the
  same node (F-ENGINE-12).

### Timers that act on state outside `obligation_policy`

`task-states.md:66-67` said that policy is "the only place a deadline may exist". Since G12 (#624) it says the
policy holds the deadlines on what a task owes and lists every timer of this table with why it is not one.

| Constant | Where | What it triggers |
|---|---|---|
| `IN_FLIGHT` 3 min | `admission.rs:43` | Reserves the memory of recent admissions; frees the slot of a return, and of a start whose launch is no longer under way (F-ENGINE-1, fixed) |
| `OPENING_TIMEOUT` 120 s (was `opening_deadline` = `io_timeout_seconds`) | `sdk_owner.rs:52` at `b479c4f7c` | Replaces a host that did not open its session (overlaps the stalled-node recovery work) |
| `RELAUNCH_WAITS`, `SILENT_HOSTS` | `sdk_owner.rs:56-59` at `b479c4f7c` | Waits before each replacement host; after 3 hosts in a row whose Claude never showed it ran, the task holds on the card that its session could not be resumed |
| `UNOWNED_TAKE_EVERY` 60 s | `sdk_owner.rs:1352` at `b479c4f7c` | Cadence of retaking tasks without an owner |
| idle_release 2 min | `idle_release.rs` | Closes the host of an idle node (documented in admission.md) |
| `UNKNOWN_RESET_WAIT_MS` 30 min | `usage_pause.rs:31,485` | An exhausted window with no reset time stops blocking |
| `TERMINATE_AFTER`/`KILL_AFTER` | `pauses.rs:20-21` | Signals to processes of paused `--bg` nodes |
| `Timing.wait`, `RETRY` | `task_closer.rs:30-55` | Forced close after the cleanup wait |
| `INTERRUPT_GRACE` 10 s | `sdk_owner.rs` (`settle_interrupt`) | How long the owner waits for a host asked to interrupt its turn and stop to close, reading its interrupted result, before killing it |
| `io_timeout` of one handoff (since #505) | `sdk_owner.rs:340-349`, `:2812` at `3fc0370ea`; `provider-claude/src/adapter.rs:672-676` | How long a stop of an SDK owner waits for the input or control it is handing to the host before asking the host to end |

At `68770a298`: `opening_deadline` is `OPENING_TIMEOUT` = 120 s, a constant of its own (`sdk_owner.rs:53`, armed at
`:1938` and `:1055`, checked at `:2298`), no longer `io_timeout_seconds` (#541). The orchestrator's `ANSWER` deadline
(10 min to remind, 20 to settle), which `obligation_policy` held and so this table never listed, was removed by #516.

At `3fc0370ea` the `sdk_owner.rs` rows above read: `opening_deadline` `:1934`, `:2042`; `RELAUNCH_WAITS`,
`SILENT_HOSTS` `:53-56`; `UNOWNED_TAKE_EVERY` `:1336`. #507 removed the idle compaction loop, which acted about two
minutes before a resting task's prompt cache expired and was not listed here (M-ENGINE-25).

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

```toml timers
[[timer]]
name = "LOOK_AT_THE_CLOCK_EVERY"
where = "crates/agent-runtime/src/owned_host.rs"
decides = "pace"
triggers = "how often an opening host's exit and the daemon's clock are looked at; the I/O timeout decides"
[[timer]]
name = "GAUGE_SAMPLE_LATE_MS"
where = "crates/daemon-server/src/views/live.rs"
decides = "pace"
triggers = "a view that reads the home gauges from a sample older than this shows them but does not keep them for the minute the views cache them; no state depends on it"
[[timer]]
name = "NO_CHECKS_GRACE"
where = "crates/core/src/automerge.rs"
decides = "state"
machine = "M-GH-3"
triggers = "a PR with no checks listed becomes mergeable once the grace ends"
[[timer]]
name = "STATUS_TIMEOUT"
where = "crates/core/src/claude_account.rs"
decides = "pace"
[[timer]]
name = "DEFAULT_CLOSE_FREE_NODES_AFTER"
where = "crates/core/src/config.rs"
decides = "state"
machine = "M-LIFE-10"
triggers = "closes a free node after it has been idle this long"
[[timer]]
name = "LAUNCH_TIMEOUT"
where = "crates/core/src/lib.rs"
decides = "pace"
[[timer]]
name = "LAUNCH_WINDOW"
where = "crates/core/src/tasks/derive.rs"
decides = "pace"
[[timer]]
name = "FIRST_RETRY_AFTER_MS"
where = "crates/core/src/update.rs"
decides = "pace"
[[timer]]
name = "SAME_WEEK_MILLIS"
where = "crates/core/src/usage.rs"
decides = "pace"
[[timer]]
name = "KEEP_DAYS"
where = "crates/daemon-engine-machine/src/machine_state.rs"
decides = "pace"
[[timer]]
name = "ROOM_WINDOW"
where = "crates/daemon-engine-machine/src/machine_state.rs"
decides = "pace"
[[timer]]
name = "WARNING_SPACING"
where = "crates/daemon-engine-machine/src/machine_state.rs"
decides = "pace"
[[timer]]
name = "HORIZON"
where = "crates/daemon-engine-machine/src/swap_disk.rs"
decides = "pace"
[[timer]]
name = "MAX_GAP"
where = "crates/daemon-engine-machine/src/swap_disk.rs"
decides = "pace"
[[timer]]
name = "WINDOW"
where = "crates/daemon-engine-machine/src/swap_disk.rs"
decides = "pace"
[[timer]]
name = "FRESH_MS"
where = "crates/daemon-engine-model/src/issues.rs"
decides = "pace"
[[timer]]
name = "CHECK_EVERY"
where = "crates/daemon-engine/src/accounts.rs"
decides = "pace"
[[timer]]
name = "DAY"
where = "crates/daemon-engine/src/antigravity_accounts.rs"
decides = "pace"
[[timer]]
name = "HOUR"
where = "crates/daemon-engine/src/antigravity_accounts.rs"
decides = "pace"
[[timer]]
name = "STOPPED_FOR_MS"
where = "crates/daemon-engine/src/cleanup.rs"
decides = "state"
machine = "M-LIFE-10"
triggers = "a node whose session stopped a day ago with no task is cleaned up"
[[timer]]
name = "STOP_WAIT"
where = "crates/daemon-engine/src/cleanup.rs"
decides = "pace"
[[timer]]
name = "GRACE"
where = "crates/daemon-engine/src/local_network_card.rs"
decides = "state"
machine = "M-ENGINE-22"
triggers = "raises the local network card once access stays denied this long"
[[timer]]
name = "TICK"
where = "crates/daemon-engine/src/local_network_card.rs"
decides = "pace"
[[timer]]
name = "PASS"
where = "crates/daemon-engine/src/obligation_watch.rs"
decides = "pace"
[[timer]]
name = "SUMMARY_EVERY"
where = "crates/daemon-engine/src/obligation_watch.rs"
decides = "pace"
[[timer]]
name = "ONE_RUN"
where = "crates/daemon-engine/src/report_held_card.rs"
decides = "pace"
[[timer]]
name = "TICK"
where = "crates/daemon-engine/src/report_held_card.rs"
decides = "pace"
[[timer]]
name = "BACKOFF_BASE_MS"
where = "crates/daemon-engine/src/usage_pause.rs"
decides = "state"
machine = "M-ENGINE-9"
triggers = "sets when a pause after a flapping return lifts"
[[timer]]
name = "BACKOFF_CAP_MS"
where = "crates/daemon-engine/src/usage_pause.rs"
decides = "state"
machine = "M-ENGINE-9"
triggers = "caps when a pause after a flapping return lifts"
[[timer]]
name = "FLAP_WINDOW_MS"
where = "crates/daemon-engine/src/usage_pause.rs"
decides = "state"
machine = "M-ENGINE-9"
triggers = "a return inside it counts as a strike and lengthens the pause"
[[timer]]
name = "JITTER_MAX_MS"
where = "crates/daemon-engine/src/usage_pause.rs"
decides = "pace"
[[timer]]
name = "UNKNOWN_RESET_WAIT_MS"
where = "crates/daemon-engine/src/usage_pause.rs"
decides = "state"
machine = "M-ENGINE-9"
triggers = "an exhausted window with no reset time stops blocking"
[[timer]]
name = "EVERY"
where = "crates/daemon-engine/src/usage_probe.rs"
decides = "pace"
[[timer]]
name = "TIMEOUT"
where = "crates/daemon-engine/src/usage_probe.rs"
decides = "pace"
[[timer]]
name = "DORMANT"
where = "crates/daemon-server/src/activity_push.rs"
decides = "pace"
[[timer]]
name = "GRACE"
where = "crates/daemon-server/src/activity_push.rs"
decides = "state"
machine = "M-SERVER-R6"
triggers = "ends the Live Activity once everything has been at zero this long"
[[timer]]
name = "MAX_BACKOFF"
where = "crates/daemon-server/src/activity_push.rs"
decides = "pace"
[[timer]]
name = "REPEAT"
where = "crates/daemon-server/src/activity_push.rs"
decides = "pace"
[[timer]]
name = "SETTLE"
where = "crates/daemon-server/src/activity_push.rs"
decides = "pace"
[[timer]]
name = "RECHECK_EVERY"
where = "crates/daemon-server/src/claude_check.rs"
decides = "pace"
[[timer]]
name = "RETRY_UPDATE_AFTER"
where = "crates/daemon-server/src/claude_check.rs"
decides = "pace"
[[timer]]
name = "DETAILS_INTERVAL"
where = "crates/daemon-server/src/client_routes.rs"
decides = "pace"
[[timer]]
name = "CLEAN_CACHE_MILLIS"
where = "crates/daemon-server/src/client_routes/pr_details.rs"
decides = "pace"
[[timer]]
name = "HEAD_PAUSE"
where = "crates/daemon-server/src/client_routes/pr_details.rs"
decides = "pace"
[[timer]]
name = "PRESENT_PAUSE"
where = "crates/daemon-server/src/client_routes/pr_details.rs"
decides = "pace"
[[timer]]
name = "BUILD"
where = "crates/daemon-server/src/client_routes/reports.rs"
decides = "pace"
[[timer]]
name = "SEND"
where = "crates/daemon-server/src/client_routes/reports.rs"
decides = "pace"
[[timer]]
name = "KEPT"
where = "crates/daemon-server/src/delivery_review.rs"
decides = "pace"
[[timer]]
name = "FLUSH_EVERY"
where = "crates/daemon-server/src/device_expiry.rs"
decides = "pace"
[[timer]]
name = "IN_USE_MS"
where = "crates/daemon-server/src/device_expiry.rs"
decides = "state"
machine = "M-SERVER-D1"
triggers = "a device used within this span is not expired"
[[timer]]
name = "RELAY_FORGOT_SPAN_MS"
where = "crates/daemon-server/src/device_expiry.rs"
decides = "state"
machine = "M-SERVER-D1"
triggers = "a device the relay keeps forgetting for this long is expired"
[[timer]]
name = "CHECK"
where = "crates/daemon-server/src/direct/bonjour.rs"
decides = "pace"
[[timer]]
name = "RETRY"
where = "crates/daemon-server/src/direct/bonjour.rs"
decides = "pace"
[[timer]]
name = "APPROVAL_TTL_MS"
where = "crates/daemon-server/src/direct/pairing.rs"
decides = "state"
machine = "M-SERVER-D3"
triggers = "an approval not collected in time expires"
[[timer]]
name = "DECIDED_TTL_MS"
where = "crates/daemon-server/src/direct/pairing.rs"
decides = "state"
machine = "M-SERVER-D3"
triggers = "a decided pairing request is forgotten"
[[timer]]
name = "PENDING_TTL_MS"
where = "crates/daemon-server/src/direct/pairing.rs"
decides = "state"
machine = "M-SERVER-D3"
triggers = "a pairing request not decided in time expires"
[[timer]]
name = "START_WINDOW_MS"
where = "crates/daemon-server/src/direct/pairing.rs"
decides = "pace"
[[timer]]
name = "AT_DESK_FOR_MS"
where = "crates/daemon-server/src/domain_delivery.rs"
decides = "pace"
[[timer]]
name = "RETRY_BACKOFF"
where = "crates/daemon-server/src/domain_delivery.rs"
decides = "pace"
[[timer]]
name = "HELLO_TIMEOUT"
where = "crates/daemon-server/src/iroh_endpoint.rs"
decides = "pace"
[[timer]]
name = "RETRY"
where = "crates/daemon-server/src/iroh_endpoint.rs"
decides = "pace"
[[timer]]
name = "SEND_PAUSE"
where = "crates/daemon-server/src/push_gate.rs"
decides = "pace"
[[timer]]
name = "UNKNOWN_MERGE_PATIENCE"
where = "crates/daemon-server/src/push_gate.rs"
decides = "pace"
[[timer]]
name = "MIN_INTERVAL"
where = "crates/daemon-server/src/push_retract.rs"
decides = "pace"
[[timer]]
name = "SETTLE"
where = "crates/daemon-server/src/push_retract.rs"
decides = "pace"
[[timer]]
name = "PROBE_EVERY"
where = "crates/daemon-server/src/reachability.rs"
decides = "pace"
[[timer]]
name = "PROBE_TIMEOUT"
where = "crates/daemon-server/src/reachability.rs"
decides = "pace"
[[timer]]
name = "REFRESH"
where = "crates/daemon-server/src/reachability.rs"
decides = "pace"
[[timer]]
name = "HEARTBEAT"
where = "crates/daemon-server/src/service/relay.rs"
decides = "pace"
[[timer]]
name = "IROH_BIND_RETRY"
where = "crates/daemon-server/src/service/relay.rs"
decides = "pace"
[[timer]]
name = "RETRY"
where = "crates/daemon-server/src/service/relay.rs"
decides = "pace"
[[timer]]
name = "STALE_READING_MS"
where = "crates/daemon-server/src/views/live.rs"
decides = "pace"
[[timer]]
name = "MAX_SKEW_MS"
where = "crates/daemon-support/src/auth.rs"
decides = "pace"
[[timer]]
name = "PAIRING_TTL"
where = "crates/daemon-support/src/auth.rs"
decides = "state"
machine = "M-SERVER-D4"
triggers = "a local pairing code expires"
[[timer]]
name = "EXPIRED_KEPT_MS"
where = "crates/daemon-support/src/device_usage.rs"
decides = "state"
machine = "M-SERVER-D1"
triggers = "the record of an expired device is forgotten"
[[timer]]
name = "RESULTS_KEPT_FOR"
where = "crates/daemon-support/src/files_retention.rs"
decides = "state"
machine = "M-GH-13"
triggers = "deletes a saved result after its task closed"
[[timer]]
name = "STORED_KEPT_FOR"
where = "crates/daemon-support/src/files_retention.rs"
decides = "state"
machine = "M-GH-13"
triggers = "deletes stored card files after the card and task ended"
[[timer]]
name = "AFTER_START"
where = "crates/daemon-support/src/local_network.rs"
decides = "pace"
[[timer]]
name = "FRESH"
where = "crates/daemon-support/src/local_network.rs"
decides = "pace"
[[timer]]
name = "EVERY"
where = "crates/daemon-support/src/node_cost.rs"
decides = "pace"
[[timer]]
name = "WINDOW"
where = "crates/daemon-support/src/node_cost.rs"
decides = "pace"
[[timer]]
name = "JWT_LIFETIME_MS"
where = "crates/daemon-support/src/push.rs"
decides = "pace"
[[timer]]
name = "EVERY"
where = "crates/daemon-support/src/relaunch.rs"
decides = "pace"
[[timer]]
name = "PREFLIGHT_TIMEOUT"
where = "crates/daemon-support/src/relaunch.rs"
decides = "pace"
[[timer]]
name = "RETRY_REJECTED"
where = "crates/daemon-support/src/relaunch.rs"
decides = "pace"
[[timer]]
name = "VERSION_TIMEOUT"
where = "crates/daemon-support/src/relaunch.rs"
decides = "pace"
[[timer]]
name = "EVERY"
where = "crates/daemon-support/src/reports.rs"
decides = "pace"
[[timer]]
name = "ONE_TRY"
where = "crates/daemon-support/src/reports.rs"
decides = "pace"
[[timer]]
name = "TTL"
where = "crates/daemon-support/src/service_files.rs"
decides = "pace"
[[timer]]
name = "EVERY"
where = "crates/daemon-support/src/updates.rs"
decides = "pace"
[[timer]]
name = "FETCH_TIMEOUT"
where = "crates/daemon-support/src/updates.rs"
decides = "pace"
[[timer]]
name = "UPGRADE_TIMEOUT"
where = "crates/daemon-support/src/updates.rs"
decides = "pace"
[[timer]]
name = "API_RETRIES"
where = "crates/domain-rules/src/api_failure.rs"
decides = "state"
machine = "M-LIFE-5"
triggers = "sends a node back by itself after an API error once the wait ends"
[[timer]]
name = "FRESH_TURN"
where = "crates/domain-rules/src/api_failure.rs"
decides = "state"
machine = "M-LIFE-5"
triggers = "a node that answered this long after an API error fails anew, not again"
[[timer]]
name = "OUTAGE_PROBE"
where = "crates/domain-rules/src/api_failure.rs"
decides = "state"
machine = "M-LIFE-5"
triggers = "one node is sent back to probe a down API"
[[timer]]
name = "QUIET_RETRY"
where = "crates/domain-rules/src/api_failure.rs"
decides = "state"
machine = "M-LIFE-5"
triggers = "a node past its retries is sent back without a card"
[[timer]]
name = "HAND_INPUT"
where = "crates/domain-rules/src/obligation_policy.rs"
decides = "state"
machine = "M-LIFE-5"
triggers = "gives up a node's process holding a handed input as hung"
[[timer]]
name = "WAKE"
where = "crates/domain-rules/src/obligation_policy.rs"
decides = "state"
machine = "M-LIFE-5"
triggers = "wakes a node that owes a wake"
[[timer]]
name = "STRIKE_GAP"
where = "crates/domain-rules/src/pr_identity.rs"
decides = "pace"
[[timer]]
name = "HOLD_LOG_EVERY"
where = "crates/engine-admission/src/admission.rs"
decides = "pace"
[[timer]]
name = "IN_FLIGHT"
where = "crates/engine-admission/src/admission.rs"
decides = "state"
machine = "M-ENGINE-1"
triggers = "frees the slot of an admitted start after it"
[[timer]]
name = "SPACING"
where = "crates/engine-admission/src/admission.rs"
decides = "pace"
[[timer]]
name = "TICK"
where = "crates/engine-admission/src/admission.rs"
decides = "pace"
[[timer]]
name = "TICK"
where = "crates/engine-admission/src/machine_watch.rs"
decides = "pace"
[[timer]]
name = "KILL_AFTER"
where = "crates/engine-admission/src/pauses.rs"
decides = "state"
machine = "M-ENGINE-2"
triggers = "kills the processes of a paused node that did not end"
[[timer]]
name = "TERMINATE_AFTER"
where = "crates/engine-admission/src/pauses.rs"
decides = "state"
machine = "M-ENGINE-2"
triggers = "terminates the processes of a paused node"
[[timer]]
name = "FIRST_BACKOFF"
where = "crates/engine-github/src/domain_merges.rs"
decides = "pace"
[[timer]]
name = "FIRST_MERGE_RETRY"
where = "crates/engine-github/src/domain_merges.rs"
decides = "pace"
[[timer]]
name = "LAST_BACKOFF"
where = "crates/engine-github/src/domain_merges.rs"
decides = "pace"
[[timer]]
name = "FIRST_SWEEP"
where = "crates/engine-github/src/open_pr_sweep.rs"
decides = "pace"
[[timer]]
name = "SWEEP_INTERVAL"
where = "crates/engine-github/src/open_pr_sweep.rs"
decides = "pace"
[[timer]]
name = "EVERY"
where = "crates/engine-kernel/src/bg_sessions.rs"
decides = "pace"
[[timer]]
name = "TURN_DEADLINE"
where = "crates/engine-kernel/src/bg_sessions.rs"
decides = "state"
machine = "M-LIFE-3"
triggers = "interrupts a claude --bg node turn that did not end"
[[timer]]
name = "EVERY"
where = "crates/engine-kernel/src/retention.rs"
decides = "pace"
[[timer]]
name = "FIRST_AFTER"
where = "crates/engine-kernel/src/retention.rs"
decides = "pace"
[[timer]]
name = "VERSION_TIMEOUT"
where = "crates/engine-kernel/src/sdk_backend.rs"
decides = "pace"
[[timer]]
name = "KEPT_MS"
where = "crates/engine-live/src/contexts.rs"
decides = "pace"
[[timer]]
name = "DEADLINE"
where = "crates/engine-mcp/src/mcp_probe.rs"
decides = "pace"
[[timer]]
name = "DEADLINE"
where = "crates/engine-mcp/src/mcp_remove.rs"
decides = "pace"
[[timer]]
name = "ANSWER"
where = "crates/engine-mcp/src/mcp_waits.rs"
decides = "pace"
[[timer]]
name = "LISTENER"
where = "crates/engine-mcp/src/mcp_waits.rs"
decides = "pace"
[[timer]]
name = "SIGN_IN_CACHE"
where = "crates/engine-mcp/src/mcp_waits.rs"
decides = "pace"
[[timer]]
name = "SWEEP_PAUSE"
where = "crates/engine-orchestrator/src/domain_orchestrator.rs"
decides = "pace"
[[timer]]
name = "CLAUDE_CACHE_MS"
where = "crates/engine-orchestrator/src/orchestrator_conversation.rs"
decides = "state"
machine = "M-ORCH-1"
triggers = "once the prompt cache lapsed, the orchestrator's next batch starts a new conversation instead of resuming"
[[timer]]
name = "STALLED_AFTER"
where = "crates/engine-orchestrator/src/domain_orchestrator/watchdog.rs"
decides = "state"
machine = "M-ORCH-1"
triggers = "declares the orchestrator stalled when its pending work made no progress"
[[timer]]
name = "WAKE"
where = "crates/engine-orchestrator/src/orchestrator_codex.rs"
decides = "pace"
[[timer]]
name = "WAKE"
where = "crates/engine-orchestrator/src/orchestrator_sdk.rs"
decides = "pace"
[[timer]]
name = "OUTAGE_WINDOW"
where = "crates/engine-ports/src/api.rs"
decides = "state"
machine = "M-LIFE-5"
triggers = "several failures inside it mean the API is down; outside it the health reads unknown"
[[timer]]
name = "ORCHESTRATOR_MESSAGE_TTL_MS"
where = "crates/engine-ports/src/journal.rs"
decides = "state"
machine = "M-ORCH-2"
triggers = "a message to the orchestrator older than this is dropped"
[[timer]]
name = "CHECK_LIMIT"
where = "crates/engine-ports/src/worktrees.rs"
decides = "pace"
[[timer]]
name = "EVERY"
where = "crates/engine-sdk/src/account_move.rs"
decides = "pace"
[[timer]]
name = "NO_RESET"
where = "crates/engine-sdk/src/account_move.rs"
decides = "state"
machine = "M-ENGINE-6"
triggers = "a task moved by a limit with no reset time stays on its account this long"
[[timer]]
name = "FIRST_BACKOFF"
where = "crates/engine-sdk/src/auto_launch.rs"
decides = "pace"
[[timer]]
name = "LAST_BACKOFF"
where = "crates/engine-sdk/src/auto_launch.rs"
decides = "pace"
[[timer]]
name = "TICK"
where = "crates/engine-sdk/src/auto_launch.rs"
decides = "pace"
[[timer]]
name = "LOOK_SPACING"
where = "crates/engine-sdk/src/idle_release.rs"
decides = "pace"
[[timer]]
name = "HALT_GRACE"
where = "crates/engine-sdk/src/sdk_owner.rs"
decides = "pace"
[[timer]]
name = "INTERRUPT_GRACE"
where = "crates/engine-sdk/src/sdk_owner.rs"
decides = "state"
machine = "M-ENGINE-4"
triggers = "kills a host that stayed open after it was asked to interrupt its turn and stop"
[[timer]]
name = "OPENING_TIMEOUT"
where = "crates/engine-sdk/src/sdk_owner.rs"
decides = "state"
machine = "M-ENGINE-4"
triggers = "replaces a host that did not open its native session"
[[timer]]
name = "RELAUNCH_WAITS"
where = "crates/engine-sdk/src/sdk_owner.rs"
decides = "state"
machine = "M-ENGINE-4"
triggers = "a new host is launched only after the wait for its failure count"
[[timer]]
name = "UNOWNED_TAKE_EVERY"
where = "crates/engine-sdk/src/sdk_owner.rs"
decides = "pace"
[[timer]]
name = "WAKE"
where = "crates/engine-sdk/src/sdk_owner.rs"
decides = "pace"
triggers = "the owner looks again on its own; what the store issues for it rings its bell sooner"
[[timer]]
name = "FIVE_MINUTES_MS"
where = "crates/engine-sdk/src/sdk_owner/session_refresh.rs"
decides = "state"
machine = "M-ENGINE-25"
triggers = "a task whose prompt cache lapsed starts a new session"
[[timer]]
name = "HOUR_MS"
where = "crates/engine-sdk/src/sdk_owner/session_refresh.rs"
decides = "state"
machine = "M-ENGINE-25"
triggers = "a task whose prompt cache lapsed starts a new session"
[[timer]]
name = "FIRST_BACKOFF"
where = "crates/engine-sdk/src/task_closer.rs"
decides = "pace"
[[timer]]
name = "LAST_BACKOFF"
where = "crates/engine-sdk/src/task_closer.rs"
decides = "pace"
[[timer]]
name = "RETRY"
where = "crates/engine-sdk/src/task_closer.rs"
decides = "pace"
[[timer]]
name = "TICK"
where = "crates/engine-sdk/src/task_closer.rs"
decides = "pace"
[[timer]]
name = "UPDATE_TIMEOUT"
where = "crates/host/src/claude_update.rs"
decides = "pace"
[[timer]]
name = "VERSION_TIMEOUT"
where = "crates/host/src/claude_update.rs"
decides = "pace"
[[timer]]
name = "FIRST_WAIT"
where = "crates/host/src/startup.rs"
decides = "pace"
[[timer]]
name = "LONGEST_WAIT"
where = "crates/host/src/startup.rs"
decides = "pace"
[[timer]]
name = "WINDOW_MILLIS"
where = "crates/host/src/startup.rs"
decides = "pace"
[[timer]]
name = "FIVE_MINUTES"
where = "crates/provider-claude/src/context.rs"
decides = "pace"
[[timer]]
name = "HOUR"
where = "crates/provider-claude/src/context.rs"
decides = "pace"
[[timer]]
name = "SAMPLES_MS"
where = "crates/store-sqlite/src/metrics_fold.rs"
decides = "state"
machine = "M-STORE-13"
triggers = "folds and deletes the samples of an ended task"
[[timer]]
name = "TASKS_MS"
where = "crates/store-sqlite/src/metrics_fold.rs"
decides = "state"
machine = "M-STORE-13"
triggers = "adds an ended task's row into its week and deletes it"
[[timer]]
name = "FRESH"
where = "crates/store-sqlite/src/owner.rs"
decides = "pace"
[[timer]]
name = "RECOVERY_WINDOW"
where = "crates/store-sqlite/src/recovery_copies.rs"
decides = "state"
machine = "M-STORE-9"
triggers = "deletes the old database kept as recovery copy"
[[timer]]
name = "WINDOW_MS"
where = "crates/store-sqlite/src/retention.rs"
decides = "state"
machine = "M-STORE-13"
triggers = "deletes journal rows older than the window"
[[timer]]
name = "LOCK_ABANDONED"
where = "crates/system/src/claude_trust.rs"
decides = "pace"
[[timer]]
name = "LOCK_WAIT"
where = "crates/system/src/claude_trust.rs"
decides = "pace"
[[timer]]
name = "PROBE_TIMEOUT"
where = "crates/system/src/codex_account.rs"
decides = "pace"
[[timer]]
name = "PROBE_TIMEOUT"
where = "crates/system/src/antigravity_account.rs"
decides = "pace"
[[timer]]
name = "VERSION_TIMEOUT"
where = "crates/system/src/antigravity_account.rs"
decides = "pace"
[[timer]]
name = "OWNER_ANSWERS_WITHIN"
where = "crates/system/src/hook_socket.rs"
decides = "pace"
[[timer]]
name = "EXCHANGE"
where = "crates/system/src/sign_in_returns.rs"
decides = "pace"
```

### M-ENGINE-11 · Machine incident (explicit: `Option<Incident>` + `step_with_disk_measurement`)

```toml state-machine
id = "M-ENGINE-11"
code = ["crates/engine-admission/src/machine_watch.rs::Incident", "crates/engine-admission/src/machine_watch.rs::step_with_disk_measurement", "crates/engine-admission/src/machine_watch.rs::look", "crates/engine-admission/src/machine_watch.rs::watch"]
truth = ["memory", "file:machine-incident.json"]
```

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

```toml state-machine
id = "M-ENGINE-12"
code = ["crates/engine-admission/src/machine_watch.rs::Declared", "crates/engine-admission/src/machine_watch.rs::waiters"]
truth = ["file:machine-incident.json", "file:admission.json"]
```

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


```toml state-machine
id = "M-ENGINE-13"
code = ["crates/engine-admission/src/disk_peaks.rs::Ledger", "crates/engine-admission/src/disk_peaks.rs::Stretch", "crates/engine-admission/src/disk_peaks.rs::Estimates"]
truth = ["file:machine-incident.json", "memory"]
```

Since `e7b898c30` only the Mac's own disk has stretches: the build-volume ledgers below are removed
(see M-ENGINE-15).

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

```toml state-machine
id = "M-ENGINE-14"
code = ["crates/engine-admission/src/machine_watch.rs::Record", "crates/engine-admission/src/machine_watch.rs::reserve_admission", "crates/engine-admission/src/machine_watch.rs::disk_room_for"]
truth = ["memory"]
```

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
  between the first and the third. See F-ENGINE-10. Since `e7b898c30` it takes it once.

### M-ENGINE-15 · Lifecycle of an observed volume (implicit, composite)


```toml state-machine
id = "M-ENGINE-15"
code = []
removed = true
```

**Removed in `e7b898c30`** by the user's decision of 2026-11-23: Alondra watches only the Mac's own
disk. `volumes`, `build_volumes`, the candidates, their stretches, grants and budgets, and the
incident's volume are gone; the fields an older daemon saved in `machine-incident.json` are ignored.
What follows describes the machine as it was audited.

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

```toml state-machine
id = "M-ENGINE-16"
code = ["crates/engine-sdk/src/bg_handover.rs::advance", "crates/engine-kernel/src/bg_sessions.rs::Handovers"]
truth = ["sqlite:sdk_handover_steps"]
states = "crates/engine-ports/src/journal/handover.rs::Step"
variants = ["Planned", "BgStopped", "SdkAdmitted", "Done"]
terminals = ["Done"]
```

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

```toml state-machine
id = "M-ENGINE-17"
code = ["crates/engine-kernel/src/launches.rs::own_worktree"]
truth = ["sqlite:task_worktrees", "sqlite:inherited_worktrees"]
```

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
read, only in the cited span: `daemon-engine/src/{domain_holds,domain_messages,idle_compaction,gate,wiring}.rs` (`idle_compaction.rs` was removed by #507),
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

```toml state-machine
id = "M-ENGINE-18"
code = ["crates/engine-kernel/src/domain_runtime.rs::DomainRuntime", "crates/engine-kernel/src/domain_runtime.rs::commit", "crates/engine-kernel/src/domain_runtime.rs::own_projected", "crates/engine-kernel/src/domain_runtime.rs::publish"]
truth = ["sqlite:command_audit", "memory"]
```

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

```toml state-machine
id = "M-ENGINE-19"
code = ["crates/engine-kernel/src/domain_runtime/seals.rs::Seals"]
truth = ["memory"]
```

- **Kind**: implicit. **Defined in**: `domain_runtime/seals.rs:9-127`.
- **Source of truth**: `clock::Monitor<Vec<Committed>>`. Each `Committed` carries dispatch and control permits,
  attempts, owner grants, recorded commands and observations (`seals.rs:96-105`).
- **States of a seal**: queued (per task) → taken by the owner of its task (`take`, `wait_for`) | forgotten
  (`forget_observations`, only observations of a host that ended, `:59-65`) | discarded (a `take` whose result is
  ignored: `engine-sdk/src/sdk_owner.rs:804`, `:900`).
- **Events**: `put` (only after publishing, `:26-38`), `take_of` (part per task), `wake` (`:100-115`), `ring_on`
  (`:44-48`).
- **Bells**: each SDK owner registers a `clock::Bell` for its execution (`Driver::new`, again on `rebind`); the
  queue keeps it weakly and drops it once the owner does. `put` rings the bells of the executions the commit issued
  something for (`executions`, `:137`), and `wake` (an owner told to stop, a turn the admission gate gave a slot)
  rings every bell. An owner waiting for its host (`readable_unless_rung`) ends that wait at once instead of after
  its `WAKE` look; a ring counts it as working before it runs, as a monitor notification does. A ring while the
  owner does something else ends its next wait at once; ringing decides nothing, the owner takes what is queued.
- **Persistence and restart**: everything is lost. The store reissues dispatch and control permits on **every**
  commit that touches the task (`store/src/lib.rs:1539-1566`: `control_permits` of all `prepared`, and the permit of
  the next offer), and `reissue_permit` on adoption (`sdk_owner.rs:785`, `:880`). That is why a lost or discarded
  seal is recovered on the next commit that touches the task (checked; see the refuted list below).
- **Concurrency**: one monitor; owners wait with `wait_for` (condvar) or on their bell while they wait for their host.
  The bells have their own mutex, taken after the monitor is released. Nothing prunes the seals of a task without an
  owner: they stay until an owner takes them (see H-ENGINE-9).

### M-ENGINE-20 · Producer capability registry (`Authority` + `domain-capabilities.json`, explicit)

```toml state-machine
id = "M-ENGINE-20"
code = ["crates/daemon-support/src/domain_authority.rs::Authority", "crates/engine-kernel/src/domain_runtime/capabilities.rs::load", "crates/engine-kernel/src/domain_runtime/capabilities.rs::persist"]
truth = ["memory", "file:domain-capabilities.json"]
```

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

```toml state-machine
id = "M-ENGINE-21"
code = ["crates/engine-kernel/src/domain_runtime/rejections.rs::Rejections"]
truth = ["memory"]
```

- **Kind**: implicit. **Defined in**: `domain_runtime/rejections.rs:7-102`.
- **States**: `(owner, interaction_id) → n`. A domain rejection (except `no_effect`) adds; a success deletes the
  entry; on reaching exactly 3 it warns the orchestrator once with id `refused-{owner}-{card}` (`tell_orchestrator`,
  idempotent by id). An accepted question or delivery withdraws those warnings.
- **Persistence and restart**: lost (the streak starts from zero). No invalid states found; entries of a card that
  never succeeds are never deleted (small growth).

### M-ENGINE-22 · Alondra's own cards (`problems.rs`, `inbox.rs`)

```toml state-machine
id = "M-ENGINE-22"
code = ["crates/engine-ports/src/runtime.rs::raise_problem", "crates/engine-ports/src/runtime.rs::withdraw_problem", "crates/engine-ports/src/runtime.rs::tell_orchestrator"]
truth = ["derived"]
```

- **Kind**: implicit. **Defined in**: `domain_runtime/problems.rs:8-199`, `inbox.rs:6-39`.
- **Idempotency**: the card id is the command id (`raise-{id}`, `tell-{id}`, `ask-{id}`, `notice-{id}`), so two
  concurrent raises of the same id resolve as a replay or an idempotency conflict in the store. `withdraw_problem`
  uses `withdraw-{id}-{revision}`.
- The prior `contains_key` check (`problems.rs:20-23` and equivalents) is only a shortcut: the store decides.

### M-ENGINE-23 · What is known live about a session (`Reported` in `Reports`, implicit, memory)

```toml state-machine
id = "M-ENGINE-23"
code = ["crates/engine-live/src/node_reports.rs::Reported", "crates/engine-live/src/node_reports.rs::Reports"]
truth = ["memory"]
```

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

```toml state-machine
id = "M-ENGINE-24"
code = ["crates/engine-live/src/node_work.rs::NodeWork", "crates/engine-live/src/node_work.rs::StepStatus"]
truth = ["memory"]
```

- **Kind**: implicit. **Defined in**: `engine-live/src/node_work.rs:91-271`.
- **States per subagent/workflow**: running (`end: None`) → finished (`Finished|Failed|Stopped`); never reopened
  (`agent_started` on a finished one only updates the description, `:112-129`). Limits: 5 finished, 20 running
  (`keep_recent`, the oldest is discarded).
- **Steps**: `step_created`/`step_updated`/`steps_listed`; `Deleted` deletes; the whole list replaces.
- Display only. No findings.

### M-ENGINE-25 · Context measure (`Contexts`, explicit; its idle compaction was removed by #507)

```toml state-machine
id = "M-ENGINE-25"
code = ["crates/engine-live/src/contexts.rs::Contexts"]
truth = ["memory", "file:context-readings.json"]
```

At the audited commit (kept for history; the current state is the last bullet):
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
- **Re-checked after #507** (at `3fc0370ea`): the idle compaction is gone (`daemon-engine/src/idle_compaction.rs`,
  `Command::CompactConversation`, `compact_guard`, `Contexts::readings`/`spend` and `LastRequest::compactable` were
  removed). What is left is a measure, with no transition that acts on its own:
  - **Defined in**: `engine-live/src/contexts.rs:56-222`; `LastRequest { tokens, requested_at, cache_ms }`
    (`engine-ports/src/sdk.rs`).
  - **Source of truth**: memory (`measured`) + `context-readings.json` (`last`), saved whole and atomically
    (`contexts.rs:100-108`); entries 7 days older than the newest request are pruned. A file written by an older
    release still loads: the extra `compactable` field is ignored, and an unreadable file loads as empty
    (`:86-91`).
  - **States of a task's last request**: unknown → measured (`observed` with `Answered`, a request time and a cache,
    `:161-173`) → deleted (`ContextEvent::Compacted`, a compaction the provider reports, `:176-179`; or `launched`
    with a `StartKind` other than `Resumed`, `:114-133`). `forget` (host ended) now only drops `measured` and keeps
    `last` (`:190-194`); before, it also marked it as not compactable.
  - **Who reads `last`**: `session_refresh::refresh` (`engine-sdk/src/sdk_owner/session_refresh.rs:122-193`,
    called inside the dispatch's handoff gate): a Claude conversation of at least 100 000 tokens whose cache expired
    and that has a pending input starts a new session through a `Refresh` provider transition instead of resuming
    (`starts_anew`, `:28-31`). Without a stored reading it derives one from the session's transcript.
  - **Upgrade**: a `Compacted` record committed before #507 keeps its staged `/compact` input, which still reaches
    the host once (M-LIFE-1).
  - No invalid state found.

### M-ENGINE-26 · Hook transport and connection of the `claude --bg` mod (`domain_transport.rs`, composite, inherited)

```toml state-machine
id = "M-ENGINE-26"
code = ["crates/daemon-engine/src/domain_transport.rs::node_connection", "crates/daemon-engine/src/domain_transport.rs::node_hold", "crates/daemon-engine/src/domain_transport.rs::hold_back", "crates/daemon-engine/src/domain_transport.rs::start_assigned"]
truth = ["sqlite:domain_outbox", "sqlite:stop_blocks", "memory"]
```

- **Kind**: composite. **Defined in**: `daemon-engine/src/domain_transport.rs:42-532`.
- **Sub-machines**:
  - **Node connection** (`node_connection :278-348`): hello → `connect_node` (number) → writes the pending outbox
    lines once per connection (`pending_lines :382-401`, per-connection `sent` set) → mod ack → `hand_over` claims
    and ends as `Handed` (`:424-436`, under `store`). A newer connection replaces the previous one (`:301-303`,
    `:321-323`).
  - **Sustained card** (`node_hold :221-272`, `answer_after_recording :165-186`): published → waiting for answer |
    withdrawn if the hook goes away (per `withdraw_when_gone`) → answered.
  - **Held end of turn** (`hold_back :642-661`): counter `stop_blocks(task) = (n, marker)` in SQLite; reset when the
    marker changes (order of the node's last card); at `n ≥ 3` it lets it stop, and the stopped node is a case for
    the orchestrator (`start_failures.rs`), never a card for the user.
  - **Task start by first activity** (`start_assigned :541-557`): `StartTask` with the read's `expected_revision`; a
    conflict is only logged and the next hook retries it.
  - Messages held for the next Stop (`DomainRuntime.held`) had no writer in production and were removed in #624
    (F-ENGINE-11).
- **Source of truth**: outbox in SQLite (M-STORE-1), `stop_blocks` in SQLite, `Reports` in memory.
- **Persistence and restart**: `stop_blocks` survives; connections and `Reports` are rebuilt when the mod
  reconnects; `release_orphans` releases at startup the sustained cards left without a hook (`:438-441`).
- **Concurrency**: everything that touches the domain goes through `hook_work` (one thread) or `guard_work` (two
  threads, read only for `PreToolUse`).

### M-ENGINE-27 · Wait report of an SDK node (`wait.json`, implicit)

```toml state-machine
id = "M-ENGINE-27"
code = ["crates/engine-live/src/sdk_live.rs::WaitReport", "crates/engine-live/src/sdk_live.rs::clear_ended_waits"]
truth = ["file:wait.json"]
```

- **Kind**: implicit. **Defined in**: `engine-live/src/sdk_live.rs:22-78`, `:755-761` (`EndedWaits`).
- **States**: absent → written by the node (`alondra report-wait`) → deleted on change of turn or execution
  (`clear_ended_waits`, in `published`, under the store lock) | ignored on read if its `pid` no longer exists.
- Display only (`read_wait_file` says so: "never an input to admission or lifecycle decisions").

### M-ENGINE-28 · Holds of an SDK task on its provider and their probe (`ClaudeHold`, explicit; added after #540)

```toml state-machine
id = "M-ENGINE-28"
code = ["crates/domain-rules/src/claude_access.rs::ClaudeHold", "crates/domain-rules/src/claude_access.rs::hold_on_claude", "crates/domain-rules/src/lifecycle.rs::hold_after_failure", "crates/daemon-engine/src/claude_access.rs::Schedule"]
truth = ["derived"]
```

At `68770a298`; this machine was outside the audited scope (M-ENGINE-9 pointed at it) and is described here because
#540 changed it.
- **Defined in**: `ClaudeHold { SignedOut, Outage, Usage }` (`domain-rules/src/claude_access.rs:42`, owners `:58-64`:
  the user for a sign-in, `Capacity(ClaudeApi)` / `Capacity(Usage)` otherwise); the hold's card
  (`SystemCard::ClaudeSignedOut | ClaudeOutage { quiet, detail, said } | ClaudeUsage { resets_at }`,
  `system_card.rs:23-36`); the task's pause in the lifecycle (`Paused { card, owner }`); the schedule
  `daemon-engine/src/claude_access.rs:20-88` (`Schedule::due`, `run`). Failures come from the provider's turn end
  (`TurnOutcome::ApiFailed(ApiFailure { error, status, resets_at, detail, said })`, `provider-contract/src/lib.rs`)
  through `hold_after_failure` (`domain-rules/src/lifecycle.rs:1544-1550`).
- **States per task**: not held → held (last hold record of its journal is `Paused` on a hold card, its lifecycle
  still waits on that `Pause` card, and the card's credential cause is current: `claude_held`, `claude_access.rs:210-226`)
  → probing (the schedule or `claude_answered` resumed it: last hold record is `Resumed`, no `TurnEnded` since;
  `last_hold`, `:140-152`) → not held when its next turn ends, or held again.
- **Transitions**: `hold_on_claude` (`:313-354`) opens a hold card and the pause; an existing pause on the same hold
  stays, an outage gives way to a sign-in or a limit (`PauseEnded` of the old card), any other pause stands.
  `probe_claude` (`:359-367`, command `ProbeClaudeAccess`) resumes the oldest held task of a wait and withdraws its
  card; `claude_answered` (`:373-387`) resumes every task the same wait held once the model answers; the user's answer
  to the card resumes that task (`settle_claude_card`, `:391-411`).
- **Probe schedule** (memory only, lost on restart, where the first probe then waits a whole interval again): one
  wait per (provider, hold, account); an outage is probed every `OUTAGE_PROBE` = 2 min, a limit at its reset or every
  `QUIET_RETRY` = 10 min (`api_failure.rs:13-16`); a sign-in never. A probe command refused (for example by a revision
  conflict) is only logged and waits for the next interval.
- **What #540 changed**:
  - An outage card of a Claude task is **quiet** until Alondra's retry failed too: `quiet = provider is Claude && !retried_on_outage(task)`
    (`:347`, `retried_on_outage` `:171-175`: the task's last hold record is `Resumed` on an outage card and its turn
    has not ended since). `shown_claude_cards` leaves quiet cards out (`:274-288`), so `grouped_away` hides them from
    the inbox (`:291-293`); `claude_waiting` still counts them. The probe still runs for a quiet card, because
    `claude_held` does not look at `quiet`. Codex keeps a loud card on the first failure.
  - The card keeps Claude Code's finer code and words (`detail`, `said`, at most 300 characters) and says "check this
    Mac's internet" when `detail` is `connection_failed` (`api_failure.rs:146-148`).
  - Claude's normalizer no longer ends a turn as failed when Claude Code wrote an API error and then the model
    answered in the same root turn (a response lost while the Mac slept): any later root assistant message without
    `error` clears it (`provider-claude/src/normalizer.rs:829-855`), and `ModelAnswered` is then emitted.
- **Persistence and restart**: the cards (with `quiet`, `detail`, `said`, stored as JSON in `system_card`) and the
  pauses live in SQLite; an outage card stored before #540 reads as a loud one with no detail (the enum is tagged by
  `kind`, the new fields default). The schedule's timing is lost.
- **Checked, no invalid state found**: a quiet card that is never probed would be a task waiting on a hidden card,
  but the probe set (`claude_held`) and the hidden set (`shown_claude_cards`) use the same credential test
  (`current_hold_credentials`), and a card with a stale credential is not hidden (it is not `grouped_away`).
  Assumed, not checked (it depends on Claude Code): that Claude Code never writes a root assistant message without
  `error` after a real, final API error of the same turn; if it did, that outage would read as a completed turn and
  open no hold.

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
**Status**: FIXED in `e7b898c30` (G4, #521). `holds_slot` keeps an admitted start's slot past `IN_FLIGHT` while its task's launch is under way; `IN_FLIGHT` alone still decides the memory reserve. Test: `engine-admission` `admission::tests::a_start_whose_launch_outlasts_three_minutes_keeps_its_slot_until_the_launch_ends`, failing with the fix removed (it admitted t2 above the limit) and passing with the fix.
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
**Status**: FIXED in `e7b898c30` (G4, #521). An `Admitted` guard in `auto_launch::pass` calls `not_started` on every exit after an Ok `ask` that does not launch. Test: `engine-sdk` `auto_launch::tests::an_automatic_start_that_gives_way_gives_its_slot_back` (an automatic start that gives way to a launch already under way), failing with the guard removed (1 slot held with no node) and passing with it.
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
**Status**: FIXED in `e7b898c30` (G4, #521), with the default decision: the pause is the domain fact `Task::paused_in_queue` (`PauseQueuedTask`, `ResumeQueuedTask`). The launch keeps its `launch_queue` row, so nothing moves between two stores; closing the task clears the fact; `queued_launches`, `still_waits`, the slot count, `live()` and `start_failures::facts` read it. A pause an older daemon kept in `admission.json` is re-queued and handed to the domain once. Tests: `domain-rules` `queue_pause_tests::cancelling_a_task_paused_in_the_queue_ends_its_pause` (fails without the clear in `close_task`), `…::a_task_is_paused_in_the_queue_once_and_only_before_it_has_a_node`, `engine-admission` `admission::tests::a_queue_pause_an_older_daemon_kept_in_the_gates_file_becomes_the_domains`, and the updated `auto_launch` pause tests.
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
**Status**: FIXED in `e7b898c30` (G4, #521). `grant` keeps the turn's age in `Saved::granted` until its owner takes the slot; `settle_turns` gives it back to a turn that waits again after a restart. Test: `engine-admission` `admission::tests::a_turn_given_a_slot_keeps_its_place_in_the_queue_across_a_restart`, failing with the fix removed (it waited since the restart) and passing with the fix.
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
**Status**: still holds at `68770a298`, and #516 widened it. #516 removed the `ANSWER` deadline from
`obligation_policy` (before the stalled-node recovery work's phase 4 did) but did not touch `task-states.md`, which
still says three deadlines are acted on, `HandInput`, `Wake` and `Answer` (`task-states.md:73-75`), and still gives
`Answer` a 10-minute deadline in its table (`:81`) and `STRANDED_AFTER` (`:186`). #541 changed `opening_deadline` into
the constant `OPENING_TIMEOUT` (120 s, `engine-sdk/src/sdk_owner.rs:53`); it is still outside the policy, so the
list of exceptions is unchanged.
- **Evidence**:
  - `docs/architecture/task-states.md:179-182` still describes `STRANDED_AFTER` (5 min, in `admission`). It was
    removed in `9abc65444` (#179, "Replace the stranded-task clock with an event") and there is no match in
    `crates/`.
  - At the same time, `:66-67` claims `obligation_policy` is the only place with deadlines, when at least
    `IN_FLIGHT`, `opening_deadline`, `RELAUNCH_WAITS`/`SILENT_HOSTS` and `UNKNOWN_RESET_WAIT_MS` (timers table
    above) decide transitions.
- **Expected fix and files**: update "Where the result differs from the plan" with the real list and decide, one by
  one, whether each deadline moves into `obligation_policy` or is documented as an exception.
- **Status: fixed** in #624 for the part not referred (checked at `b479c4f7c`: `task-states.md` still described
  `STRANDED_AFTER` and the `Answer` deadline, and claimed the policy was the only place with deadlines).
  `task-states.md` now says the policy holds `HandInput` and `Wake` (both enforced), that what the orchestrator owes
  has no deadline since #516, and lists every engine timer outside the policy with what it does and why it is not a
  deadline on what a task owes: `IN_FLIGHT` (memory estimate; a start keeps its slot until its node exists or its
  launch ends since #556), `OPENING_TIMEOUT` (owned by the stalled-node recovery work), `RELAUNCH_WAITS` and
  `SILENT_HOSTS` (a back-off and a count of ended hosts, not a time a step may take; the one rule there that opens a card), `UNOWNED_TAKE_EVERY` (a cadence, as decided), idle release,
  `UNKNOWN_RESET_WAIT_MS`, `TERMINATE_AFTER`/`KILL_AFTER`, the closer's wait and `RETRY`, and the handoff I/O timeout.
  `STRANDED_AFTER` is gone from the doc (removed in #179).

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
**Status**: FIXED in #613. `adopt_for_transition` now takes the task's `TakeoverTurn`, checks `owned_now` inside
it and adds its owner before releasing it, so it never overlaps `take_again`, `take_over_unowned` or any other
takeover of the task: whichever runs second finds the host owned and does nothing. Test:
`sdk_owner::codex_tests::a_model_change_during_a_retake_waits_for_it_and_never_reads_the_host_as_lost` (a barrier
holds the retake right after it reserved its owner, the model change runs, the barrier opens). Without the fix,
on `e1d5ec2aa`, the change adopts the host and the retake fails with «the reservation issued no owner grant»: the
change took the retake's grant from the task's seals, the variant noted below as not followed, now seen. The
retake then gives the host up as lost (`lost()`, `host_losses_told`) while the change's owner works with it,
which is branch 1. The text below is the audit, kept as evidence. Earlier status: still holds at `3fc0370ea`. #505 changed only the
`Owner`'s stop, not how hosts are taken, so the exclusion gaps below are unchanged. New evidence from #505: the handoff gate belongs to each `Owner`
(M-ENGINE-4), so in branch 2 a close stops the first live `Owner` (`take_sdk_owner`, `sdk_owner.rs:3074-3079`),
whose `Owner::end` waits only for its own handoffs and then asks the shared host to end (`:2812-2818`) while the
second `Owner` may be handing it an input. That is the uncertain receipt #505 removed for the single-owner case,
and the task's end can again be refused with `UnresolvedDispatch` until a retry (not followed further than
`Owner::end`). Refreshed refs at `3fc0370ea` (old →
new): `:130` → `:131`; `stale()` `:2488-2491` → `:2565-2568`, its uses `:1763`, `:1805`, `:1833`, `:1842`, `:1853` →
`:1830`, `:1872`, `:1905`, `:1914`, `:1928`; `TakeoverTurn` in `take_again` `:1409` → `:1471`; `retake_with_policy`
`:1384-1385` → `:1446-1447`; `halt` `:1479-1480` → `:1541-1542`; `adopt_for_transition` `:763` → `:825`;
`owns_sdk_host` `:2886-2891` → `:2968-2973`; `add_sdk_owner` `:2925-2934` → `:3007-3016`; `take` `:852-853` →
`:914-915`; reservation `:876-885` → `:938-947`; `adapter_state` `:899` → `:961`; `not_taken` → `lost()`
`:1155-1157` → `:1217-1219`; `host_ended` in `lost` `:1208-1211` → `:1270-1273`; paused `:1212-1215` →
`:1274-1277`; `RecordHostLost` `:1217-1224` → `:1279-1286`; `Found::Live` `:915-918` → `:977-980`; `adopt_owner`
`:939` → `:1001`; `:966-991` → `:1028-1053`; `add_sdk_owner(A)` in `take_again` `:1465-1466` → `:1527-1528`;
`ask_to_release_host` `:2936-2940` → `:3018-3022`; `end_holders` `:939-947` → `:1001-1009`; `seals.take`
`:885-887` → `:947-949`; `after_owner_reserved` `:2954-2960` → `:3036-3042`; the domain rule of `RecordHostLost`
`rules.rs:5540-5545` → `:5552-5557`.
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
**Status**: FIXED in `e7b898c30` (G4, #521). With the user's decision of 2026-11-23 no other volume is watched, so the variant with volumes is gone; `pace` returns whether the sample was new and `step_with_disk_measurement` moves `healthy` only on a new one. Test: `engine-admission` `machine_watch::tests::a_second_look_at_the_same_sample_moves_no_recovery`, failing with the fix removed (the second look closed the incident) and passing with the fix.
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
**Status**: NO LONGER APPLIES since `e7b898c30` (G4, #521). The user decided on 2026-11-23 that Alondra watches only the Mac's own disk: build volumes, their stretches and their reservations are removed, so there is nothing left to lose on a restart. The Mac disk's stretches are saved as before (`what_each_project_takes_and_what_a_node_at_work_took_are_kept_across_a_restart`).
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
**Status**: FIXED in group G1 (PR #548). `own_worktree` reads whether the task ended after trying to record the worktree, and an ended task's worktree is released at once with `task_not_active` instead of `storage_unavailable` (`crates/engine-kernel/src/launches.rs`). Test: `a_worktree_made_for_a_task_cancelled_while_it_started_goes_at_once` (`//crates/engine-sdk:engine_sdk_engine_test`). Ablation: with `own_worktree` as on `main` it fails with `storage_unavailable` and the worktree stays; restored, it passes.
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
**Status**: NO LONGER APPLIES since `e7b898c30` (G4, #521). The user decided on 2026-11-23 that Alondra learns no build volume: `measure_volumes`, `build_volumes`, `volumes`, the candidates and the `Disk` port's mount table are removed, and the fields an older daemon saved are ignored. No volume other than the Mac's own disk can hold a start or a turn.
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
**Status**: FIXED in `e7b898c30` (G4, #521) as a structural fix, which the finding allowed: `disk_room_for` and `disk_detail_for` read the reservations (`Record::reserved`) and decide `alone` under one lock, so a sample can no longer clear `disk_working` between the steps. There is no interleaving left for a test to reproduce; `the_head_alone_needs_only_the_line_of_the_disk` and `the_head_excludes_forecasts_but_not_earlier_grants` pin the result.
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
- **Status: fixed** in #624, commit `dfcca016a` (still held at `b479c4f7c`). Removed: `DomainRuntime.held` and the `held()` method of
  the `Runtime` port and its doubles, `take_held` and the Stop branch that returned `additionalContext`, the
  orchestrator read's `messages_held_for_turn_end` (and its display in `alondra mcp`), the `blocks > STOP_BLOCKS`
  withdrawal and `undelivered_problem_id`. The comments of `STOP_BLOCKS` and `hold_back` now say the capped node is a
  case for the orchestrator. The test that wrote into `held` by hand (`a_held_user_message_reaches_the_next_stop_hook_once`)
  went with it. No ablation applies: the removed branches could not run, which the build proves (nothing outside tests
  wrote `held`).

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
  race was confirmed as F-ENGINE-12, whose branch 1 reached `lost()` → `host_ended(agent)` while another take
  held the host. **Since #613** (F-ENGINE-12 fixed) every takeover of a task, the model change's included, runs in
  its `TakeoverTurn`, and the `lost()` of a failed take runs inside that turn (`not_taken` in `take_again` and
  `take_over_unowned`), so that path no longer reaches another take's window. Left to check: whether a `lost()` from
  an `Owner`'s own failure (`sdk_owner.rs`, the `replace_ended_host` and `lost_because` paths, which run outside
  any turn with the owner already reading as ended) can overlap a takeover started by something that checks only
  `owns_sdk_host` (`provider_transition`'s `retake` after a move, a retake from the host-lost card). Not tested,
  so it stays a hypothesis. What remains to check is the rest of H-ENGINE-8 (that
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
- **The close of #505 waits forever for a handoff** (checked at `3fc0370ea`): it does not. `Owner::end` waits only
  while one `dispatch` or `control` call runs (`sdk_owner.rs:340-349`); the provider bounds that call's write and
  receipt by `io_timeout` (`provider-claude/src/adapter.rs:672-676`, at most 300 s), no stopper holds the store or
  `Owners.held` while it waits, and the owner thread never stops its own `Owner` (the stops a gate pass or a commit
  reaches run on threads of their own: `close_hosts_of_ended_nodes`, `sdk_owner.rs:1369-1373`, and `halt`,
  `:1552-1560`). A permit refused by a
  closed gate is put back unclaimed, so no claim is left open by it.

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

```toml state-machine
id = "M-STORE-1"
code = ["crates/domain-store-schema/schema.sql::domain_outbox", "crates/core/src/delivery.rs::State", "crates/store-sqlite/src/lib.rs::claim_in", "crates/store-sqlite/src/lib.rs::finish_message"]
truth = ["sqlite:domain_outbox", "sqlite:delivery_attempts", "sqlite:domain_message_user_targets", "sqlite:domain_messages"]
```

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
  **Enforced since G3**: only one daemon opens the database as writer (`daemon.lock`, F-STORE-1).
- **Persistence and restart**: `dispatching` → `uncertain` (never redelivered just because of a restart). Delivery to
  an SDK node is idempotent by `command_id = tell-outbox-{message.id}` while the receipt lives (one week). User push:
  `uncertain` counts as already notified (`user_push_handed`, `lib.rs:1584`).
- **Concurrency**: everything under the store `Mutex` (`DomainRuntime.store`) and `IMMEDIATE` transactions.

### M-STORE-2 · Command commit and idempotency (`command_audit`, `semantic_receipts`, `local_command_results`, `command_bodies`)

```toml state-machine
id = "M-STORE-2"
code = ["crates/store-sqlite/src/command.rs::stage", "crates/store-sqlite/src/retention.rs"]
truth = ["sqlite:command_audit", "sqlite:semantic_receipts", "sqlite:local_command_results", "sqlite:command_bodies", "sqlite:legacy_schedule_receipts"]
```

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

```toml state-machine
id = "M-STORE-3"
code = ["crates/domain-rules/src/read_index.rs::ReadSnapshot", "crates/engine-kernel/src/domain_runtime.rs::publish"]
truth = ["derived"]
```

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

```toml state-machine
id = "M-STORE-4"
code = ["crates/store/src/lib.rs::DispatchPermit", "crates/store/src/lib.rs::recover_prepared", "crates/store/src/lib.rs::unresolved_commands", "crates/store/src/receipt_proofs.rs"]
truth = ["sqlite:lifecycle_dispatches", "sqlite:provider_receipt_proofs"]
```

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

```toml state-machine
id = "M-STORE-5"
code = ["crates/store/src/lib.rs::accept_observation", "crates/store-sqlite/src/discarded_observations.rs"]
truth = ["sqlite:provider_observations", "sqlite:provider_observation_heads", "sqlite:provider_normalization_checkpoints", "sqlite:provider_discarded_observations", "sqlite:task_context_gaps", "sqlite:task_context_items", "sqlite:task_context_pins", "sqlite:task_context_usage"]
```

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

```toml state-machine
id = "M-STORE-6"
code = ["crates/store-sqlite/src/batch_effects.rs::record_batch_report", "crates/store-sqlite/src/batch_effects.rs::finish_batch", "crates/store-sqlite/src/batch_recovery.rs"]
truth = ["sqlite:orchestrator_batch_runs", "sqlite:orchestrator_batch_effects", "sqlite:orchestrator_batch_reports", "sqlite:orchestrator_tool_operations", "sqlite:orchestrator_batch_decisions", "sqlite:orchestrator_deliveries"]
```

- **Defined in**: `crates/store-sqlite/src/batch_effects.rs:6-12`, `batch_recovery.rs`.
- **States**: run `ended 0|1`; tool operation `started → succeeded | uncertain`; reports with a gapless cursor
  (`record_batch_report`, `batch_effects.rs:168-188`); effects pin their receipts (`pinned`) until `finish_batch`.
- The recovery logic (what the orchestrator does with these states on restart) belongs to the orchestrator area.

### M-STORE-7 · Local action journal (`action-results.sqlite`) and operation journal (`operation_journal`)

```toml state-machine
id = "M-STORE-7"
code = ["crates/daemon-server/src/views/action_results.rs::recover_interrupted", "crates/daemon-server/src/operation_journal.rs"]
truth = ["sqlite:action_results", "sqlite:operation_meta", "sqlite:operation_issuers", "sqlite:operation_retired"]
```

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

```toml state-machine
id = "M-STORE-8"
code = ["crates/daemon-server/src/service.rs::recover_interrupted"]
truth = ["sqlite:service_receipts", "sqlite:attempts"]
```

- **Defined in**: `crates/daemon-server/src/service.rs:1190-1325, 2925-3020`.
- **States**: `accepted → executing → completed`; `executing → interrupted` (startup); `interrupted` with the same
  digest → back to `executing` but answered "uncertain" (409) except reads; `retired`. Concurrent duplicates are
  filtered by the `inflight` map.

### M-STORE-9 · Opening and migrating `domain.db`

```toml state-machine
id = "M-STORE-9"
code = ["crates/store-sqlite/src/domain_database_migration.rs::Phase", "crates/store-sqlite/src/domain_database_migration.rs::migrate_and_fence", "crates/store-sqlite/src/lifecycle_migration.rs"]
truth = ["file:database-migration.json", "derived", "sqlite:domain_meta", "sqlite:lifecycle_journal_format"]
```

- **Defined in**: `crates/store-sqlite/src/lib.rs:605-759` (`open`), `lifecycle_migration.rs` (schema 4→5, all in one
  transaction with a prior copy), `domain_database_migration.rs` (rename `domain-v2.db`→`domain.db`, marker
  `database-migration.json` Prepared/Committed with digests and folder fsync).
- **States**: `user_version` ∈ {0 (new), PREVIOUS (migrates), SCHEMA (opens)}; anything else → rejects, preserving the
  database. Schema migration and `user_version` in the same transaction: there is no half migration.
- **Restart recovery included in every `open`** (not only at daemon start): `delivery_attempts/domain_outbox`
  `dispatching→uncertain`, `schedule_runs launching→interrupted`, `launch.state dispatching→uncertain`
  (`lib.rs:724-735`).

### M-STORE-10 · Startup sequence (composite)

```toml state-machine
id = "M-STORE-10"
code = ["crates/alondrad/src/lib.rs::run", "crates/alondrad/src/assemble.rs", "crates/daemon-support/src/domain_cutover.rs::Phase"]
truth = ["derived"]
```

`crates/alondrad/src/lib.rs:121-158` and `crates/alondrad/src/assemble.rs:180-300`:
1. `home_renames::apply`, `refuse_legacy_home`, `migrate_and_fence` (exclusive lock only if there is a marker or a
   cutover), `retire_empty_older_sidecar`;
2. `HomeLock::shared` (shared with the CLI) and `ensure_ready`;
3. `HomeLock::daemon` (exclusive `daemon.lock`: a second daemon stops here, F-STORE-1), then `wiring::open` → `DomainStore::open` (recovery of M-STORE-9) + `recover_settings` + `fill_task_*` + `load` (replay)
   + `capabilities::load` + commands `ReviewWaits` and `AcknowledgeRequestTasks`;
4. `DomainDaemon::over` (`Auth::open`);
5. `assemble`: `service.recover_interrupted()` and `action_results::recover_interrupted` (`assemble.rs:187-190`),
   **before** `TcpListener::bind` (`assemble.rs:259`); SDK adoption (`prepare_workers`) after the bind.
- Only processes supervised by launchd wait for the home folder to become free (`lib.rs:89-91`,
  `relaunch::wait_until_free`).

### M-STORE-11 · Append-only step journals

```toml state-machine
id = "M-STORE-11"
code = ["crates/store-sqlite/src/provider_transition.rs", "crates/store-sqlite/src/account_move.rs", "crates/store-sqlite/src/handover.rs"]
truth = ["sqlite:provider_transition_steps", "sqlite:account_move_steps", "sqlite:sdk_handover_steps"]
```

- `provider_transition_steps` (`crates/store-sqlite/src/provider_transition.rs:9-75`): phases
  `requested → draining → source_fenced → context_ready → target_admitted → target_ready → consumed → done`, with
  shortcuts, `aborted`/`uncertain` from any non-terminal, and **`done → start_refused → done`** (a terminal that
  reopens, only with `deferred_start`). One open per task (trigger).
- `account_move_steps` (`account_move.rs`): planning is disabled (`bail!`), only old moves are read.
- `sdk_handover_steps` (`handover.rs`): `planned → bg_stopped → sdk_admitted → done`, order by trigger.

### M-STORE-12 · State in JSON files in the home folder (implicit machines split with SQLite)

```toml state-machine
id = "M-STORE-12"
code = ["crates/daemon-engine/src/live.rs", "crates/engine-admission/src/admission.rs"]
truth = ["file:closed-sessions.json", "file:aborted-replaces.json", "file:orchestrator.json", "file:admission.json", "file:domain-capabilities.json"]
```

Inventory (helpers: `foundation/src/save.rs:9` `save::atomic` does fsync of the file, not of the folder;
`durability::flush` is used by the hand-written writers):

| File | Write | Also in SQLite? | Reconciliation at startup |
|---|---|---|---|
| `closed-sessions.json`, `aborted-replaces.json` (`daemon-engine/src/live.rs:22-29`) | tmp+rename **without fsync**; an error is only logged | No | None; unreadable → empty silently (`live.rs:107-113`) |
| ~~`closed-task-nodes.json`~~ (until G2, #552) | — | Replaced by `Task.released` in `task_released_nodes`, written by the commit of `ReleaseTask`/`RecordNodeEnd` | A daemon that starts deletes the old file; a task whose entry it held is caught by `holder()` (F-STORE-4) |
| `orchestrator.json` (`engine-orchestrator/src/domain_orchestrator.rs:125-131`) | tmp+rename without fsync | Yes, after the batch claim (`:509` → `:528`) | Yes, `repair_incomplete` (`domain_orchestrator/recovery.rs:26`); unreadable → retries every 5 s |
| `admission.json` (`engine-admission/src/admission.rs:304-310`) | `save::atomic` | Partial (pauses in SQLite first) | `pauses.rs:111-133` prunes `owed/resumed` |
| `sdk/<instance>/launch-options.json` (`daemon-engine-model/src/launch.rs:136`) | `save::atomic` | Partial (`selection`), JSON before the commit (`engine-sdk/src/provider_transition.rs:472-473`) | Not confirmed |
| `domain-capabilities.json` (`engine-kernel/src/domain_runtime/capabilities.rs:93-127`) | atomic with folder fsync | Derived; pruned against SQLite on load | Yes; unreadable → **does not start** |
| `options.toml` + `options.pending` (`engine-kernel/src/settings_projection.rs`) | two phases with a receipt in SQLite | Yes | Yes, `provider_settings::recover_store` |
| `options.toml` via `set_node_limit` (`engine-admission/src/admission.rs:1116`) | tmp+rename without fsync, without looking at `options.pending` | No | — |
| `database-migration.json`, `service-recovery.json` | atomic with folder fsync | — | Yes |
| `config.toml` | tmp+rename without fsync | No | unreadable → does not start |
| `startup-failures.json`, `auto-update.json` (in-place write), several device files | without fsync | No | by default |

```toml state-files
[[file]]
name = "aborted-replaces.json"
writer = "crates/daemon-engine/src/live.rs"
machine = "M-LIFE-9"
[[file]]
name = "admission.json"
writer = "crates/engine-admission/src/admission.rs"
readers = ["crates/engine-ports/src/admission.rs", "crates/daemon-server/src/preflight.rs"]
machine = "M-ENGINE-1"
[[file]]
name = "apns.json"
writer = "crates/cli/src/setup.rs"
readers = ["crates/cli/src/doctor/system.rs", "crates/daemon-support/src/push.rs"]
machine = "M-SERVER-D8"
[[file]]
name = "auto-update.json"
writer = "crates/daemon-support/src/updates.rs"
machine = "M-SERVER-D9"
[[file]]
name = "closed-sessions.json"
writer = "crates/daemon-engine/src/live.rs"
machine = "M-LIFE-9"
[[file]]
name = "config.toml"
writer = "crates/core/src/config.rs"
readers = ["crates/engine-sdk/src/sdk_owner/codex_fixtures.rs", "crates/engine-sdk/src/sdk_owner/node_environment.rs", "crates/system/src/codex_account.rs"]
machine = "M-STORE-12"
[[file]]
name = "context-readings.json"
writer = "crates/engine-live/src/contexts.rs"
machine = "M-ENGINE-25"
[[file]]
name = "database-migration.json"
writer = "crates/store-sqlite/src/domain_database_migration.rs"
machine = "M-STORE-9"
[[file]]
name = "device-languages.json"
writer = "crates/daemon-server/src/client_routes.rs"
machine = "M-SERVER-D8"
[[file]]
name = "device-usage.json"
writer = "crates/daemon-support/src/device_usage.rs"
machine = "M-SERVER-D1"
[[file]]
name = "devices.json"
writer = "crates/daemon-support/src/auth.rs"
readers = ["crates/cli-report/src/report/private.rs", "crates/cli/src/service_pair.rs", "crates/daemon-server/src/preflight.rs"]
offline_writers = ["crates/cli/src/service_setup/recovery.rs"]
offline_why = "service recovery rewrites it under the home lock while the daemon is stopped"
machine = "M-SERVER-D1"
[[file]]
name = "direct-denied.json"
writer = "crates/daemon-server/src/direct/pairing.rs"
machine = "M-SERVER-D3"
[[file]]
name = "direct-devices.json"
writer = "crates/daemon-server/src/direct.rs"
machine = "M-SERVER-D3"
[[file]]
name = "direct-iroh.json"
writer = "crates/client-transport/src/iroh_client.rs"
machine = "M-CLIENTS-R7"
[[file]]
name = "direct-macs.json"
writer = "crates/client-transport/src/direct_client.rs"
machine = "M-CLIENTS-R7"
[[file]]
name = "domain-capabilities.json"
writer = "crates/engine-kernel/src/domain_runtime/capabilities.rs"
readers = ["crates/daemon-server/src/preflight.rs", "crates/engine-sdk/src/domain_schedule_runner.rs"]
machine = "M-ENGINE-20"
[[file]]
name = "iroh-terminals.json"
writer = "crates/daemon-server/src/iroh_terminals.rs"
machine = "M-SERVER-D7"
[[file]]
name = "launch-options.json"
writer = "crates/daemon-engine-model/src/launch.rs"
readers = ["crates/cli-report/src/report/collect.rs"]
machine = "M-ENGINE-7"
[[file]]
name = "live-activity.json"
writer = "crates/relay/src/activity.rs"
machine = "M-SERVER-R6"
[[file]]
name = "machine-incident.json"
writer = "crates/engine-admission/src/machine_watch.rs"
readers = ["crates/cli-report/src/report/collect.rs"]
machine = "M-ENGINE-11"
[[file]]
name = "native-account.json"
writer = "crates/daemon-engine-model/src/account_identity.rs"
readers = ["crates/engine-sdk/src/host_folders.rs"]
machine = "M-PROV-12"
[[file]]
name = "native-auth-account.json"
writer = "crates/daemon-engine-model/src/account_identity.rs"
readers = ["crates/system/src/codex_account.rs"]
machine = "M-PROV-12"
[[file]]
name = "options.pending"
writer = "crates/daemon-engine-model/src/options.rs"
readers = ["crates/daemon-server/src/preflight.rs"]
machine = "M-PROV-12"
[[file]]
name = "options.toml"
writer = ["crates/engine-kernel/src/settings_projection.rs", "crates/engine-admission/src/admission.rs"]
readers = ["crates/core/src/options.rs", "crates/daemon-server/src/preflight.rs"]
machine = "M-PROV-12"
[[file]]
name = "orchestrator.json"
writer = "crates/engine-orchestrator/src/domain_orchestrator.rs"
readers = ["crates/daemon-server/src/preflight.rs"]
machine = "M-ORCH-1"
[[file]]
name = "owner.json"
writer = "crates/process-group/src/owner.rs"
readers = ["crates/engine-orchestrator/src/orchestrator_codex.rs", "crates/engine-orchestrator/src/orchestrator_sdk.rs"]
machine = "M-ORCH-1"
[[file]]
name = "public.json"
writer = "crates/cli/src/service_setup.rs"
readers = ["crates/core/src/home_lock.rs"]
machine = "M-SERVER-D1"
[[file]]
name = "revoked-devices.json"
writer = "crates/daemon-support/src/revoked_devices.rs"
machine = "M-SERVER-D2"
[[file]]
name = "service-keys/public.json"
writer = "crates/cli/src/service_setup.rs"
readers = ["crates/core/src/home_lock.rs"]
machine = "M-SERVER-D1"
[[file]]
name = "service-pairing-pending.json"
writer = "crates/cli/src/service_pair.rs"
readers = ["crates/cli/src/service_setup/recovery.rs"]
machine = "M-SERVER-D12"
[[file]]
name = "service-recovery.json"
writer = "crates/cli/src/service_setup/recovery.rs"
readers = ["crates/core/src/home_lock.rs"]
machine = "M-SERVER-D1"
[[file]]
name = "sending.json"
writer = "crates/cli-report/src/report/send.rs"
readers = ["crates/cli-report/src/report/bundle.rs", "crates/daemon-support/src/reports.rs"]
machine = "M-CLIENTS-R14"
[[file]]
name = "service.toml"
writer = "crates/cli/src/service_setup.rs"
readers = ["crates/cli-report/src/report/mod.rs", "crates/cli/src/commands.rs", "crates/cli/src/doctor/system.rs", "crates/cli/src/service_pair.rs", "crates/core/src/home_lock.rs", "crates/core/src/service_config.rs", "crates/daemon-server/src/domain_delivery.rs", "crates/daemon-server/src/service.rs"]
offline_writers = ["crates/daemon-server/src/service/scenario_phone.rs"]
offline_why = "the scenario phone writes it only in the end-to-end sandbox, never in an installed daemon"
machine = "M-SERVER-D1"
[[file]]
name = "startup-failures.json"
writer = "crates/host/src/startup.rs"
machine = "M-SERVER-D10"
[[file]]
name = "terminal-service.json"
writer = "crates/client-transport/src/service_client.rs"
machine = "M-CLIENTS-R9"
[[file]]
name = "wait.json"
writer = "crates/cli/src/report_wait.rs"
readers = ["crates/engine-live/src/sdk_live.rs"]
machine = "M-ENGINE-27"
```

### M-STORE-13 · Retention

```toml state-machine
id = "M-STORE-13"
code = ["crates/store-sqlite/src/retention.rs", "crates/store/src/retention.rs"]
truth = ["derived", "sqlite:retention_marks"]
```

`crates/store-sqlite/src/retention.rs` (bodies and receipts 7 days, except receipts pinned by batches; message texts of
finished tasks) and `crates/store/src/retention.rs` (observation bytes, commands of non-current executions, texts of
inputs of finished tasks except `uncertain` dispatches). Each pass in its own transaction; none of it is domain state
(the cache is kept).

### M-STORE-14 · Relaxed durability (`alondra_foundation::durability`, PR #493)

```toml state-machine
id = "M-STORE-14"
code = ["crates/foundation/src/durability.rs"]
truth = ["derived"]
```

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
**Status**: FIXED in `2fc1e9349` (G3, #520). The daemon takes `daemon.lock` exclusively (`HomeLock::daemon`) before `wiring::open` (`alondrad/src/lib.rs`, `open_as_writer`), and `--test-service-push` no longer opens the domain database. Test: `alondrad` `startup_tests::a_second_daemon_is_refused_before_its_restart_recovery_touches_the_live_daemons_deliveries`. Recovery stays inside `DomainStore::open`, where the tests that reopen a store to simulate a restart rely on it; the lock makes the daemon its only production caller on a live HOME.
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
**Status**: FIXED in `2fc1e9349` (G3, #520). The part this group owns: `preflight.rs` copies the startup's JSON and TOML state files into the snapshot (`STATE_FILES`, bytes and permission bits). Test: `daemon-server` `preflight::tests::preflight_refuses_a_state_file_the_real_startup_cannot_read`. The binary smoke test remains referred.
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
**Status**: FIXED in `2fc1e9349` (G3, #520). A retry of an expired orchestrator message is refused with `delivery_expired` before anything changes (`resolve_uncertain_authenticated`). Test: `store-sqlite` `tests::retrying_an_uncertain_orchestrator_message_past_its_24_hours_is_refused_and_discarding_it_works`.
**Class**: CONFIRMED · **Severity**: S3 · **Probability**: medium (nobody checks `alondra delivery list` daily) ·
**Overlap**: orchestrator area (TTL of its inbox); new evidence on the interaction with `alondra delivery`.
the stalled-node recovery work (phase 4) changes the `ANSWER` deadline of the orchestrator inbox, not the 24 h expiry of the
outbox. No open issue.
**Status**: still holds at `68770a298`. #516 removed the `ANSWER` deadline itself but left the 24 h expiry and the
retry path as they were (its change to `daemon-server/src/domain_delivery.rs` is in a test). One more effect: a
reminder of #516 whose message is discarded this way never counts as read (`orchestrator_notice_read` requires
`handed`, `store-sqlite/src/lib.rs:1554-1557`), so the next settle sends a numbered new one instead of acting on it.
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

#### F-STORE-7 · A request cleared and then taken back by its task leaves its «cleared» row, and the daemon no longer loads its state
**Status**: FIXED in `2fc1e9349` (G3, #520). Saving a request whose `cleared` went from true to false deletes its
`cleared_requests` row (`store-sqlite/src/repository.rs`). Test: `store-sqlite`
`tests::a_cleared_request_its_ended_task_takes_back_loads_open_after_a_restart`.
**Class**: CONFIRMED (reproduced on `main` 093f26be0 by the test above: the reload failed with
`invalid_input: Cleared Request is still open`) · **Severity**: S1 (every later command fails and the daemon does not
start again until `domain.db` is edited by hand) · **Probability**: low · **Overlap**: found by the audit node after
#530; #536 added a second writer of `cleared = false`.
- **Invalid state/transition and broken invariant**: a request open in memory and «cleared» in SQLite; `validate`
  refuses the loaded state («Cleared Request is still open»).
- **Evidence**: the save only inserted into `cleared_requests` when `cleared` is true and never deleted
  (`repository.rs`), while the load sets `cleared = true` for every row. `AdoptOpenPr` (`domain-rules/src/rules.rs`)
  and, since #536, `reopen_requests_with_open_tasks` write `cleared = false`.
- **Minimal sequence**: 1) a request is cleared while its last task is still ending; 2) the task ends with its pull
  request open; 3) the open-PR sweep runs `AdoptOpenPr`: in memory `closed = false`, `cleared = false`, the row stays;
  4) the next full reload (routine, after each launch dequeue) reads it cleared and open and refuses it.
- **Deterministic test and ablation**: the store test registers a PR, cancels the task, closes and clears its request,
  ends the node, adopts the PR and reopens the database. Ablation: drop the `DELETE` and the reload fails.

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
     (`domain-rules/src/obligation_policy.rs:15-27`), and a queued task owes none of them. Re-checked at `68770a298`:
     since #516 it enforces only `HandInput` and `Wake` (`obligation_policy.rs:24-26`); the step still holds.
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
- **Status: fixed** in #552 (commit `56d796f26`); confirmed on `main` at `10ebafced` before the change (the entry
  was still written by `remember_closed_task` after the commit, and `holder()` still had no production caller).
  The closed node is `Task.released`, written by the commit that releases the task (table `task_released_nodes`), and
  `DomainState::react` turns an `Ownerless` released task with no record into the `NodeClosed` card. Final behaviour
  and evidence: [G2](#g2--a-released-task-always-has-a-way-back). Ablations: removing the write of `released` from
  `ReleaseTask` makes `tests::a_released_task_keeps_who_starts_its_next_node_in_the_commit_that_releases_it` fail, and
  makes the engine test after a restart fail: the restarted daemon no longer relaunches the task by itself; removing the `holder()` branch
  of `react` makes `ownership_tests::a_released_task_with_no_record_of_its_closed_node_is_put_to_the_user` fail with no
  reaction.

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

```toml state-machine
id = "M-GH-1"
code = ["crates/domain-rules/src/model.rs::PrState", "crates/domain-rules/src/model.rs::PullRequest", "crates/core/src/pr.rs::CheckState", "crates/core/src/pr.rs::PrState"]
truth = ["sqlite:pull_requests", "sqlite:pull_request_tasks", "sqlite:pull_request_aliases", "sqlite:pr_notes_from_github", "sqlite:pull_request_reviews", "sqlite:merge_verdicts"]
states = "crates/domain-rules/src/model.rs::PrState"
variants = ["Open", "Closed", "Merged"]
terminals = ["Merged"]
```

- **Kind**: explicit (enum `PrState {Open, Closed, Merged}`) plus the fields `head`, `base`, `merged_head`,
  `allowed_methods`, `review`. **Defined in**: `crates/domain-rules/src/model.rs:517-625`. Transitions in
  `rules.rs:5095-5180` (`ObservePrState`), `rules.rs:4597-4727` (`RegisterPr`, including the move between tasks) and
  `rules.rs:4728-4760` (`NotePrOpened`/`RecordOpenedPr`).
- **Source of truth**: GitHub. **Copies and derivatives**: `DomainState.pull_requests` (table `pull_requests`; owners in
  `pull_request_tasks`; history in `pr_task_history`); `Task.pr_id`/`later_prs`; the server's in-memory PR details cache
  (`daemon-server/src/client_routes/pr_details.rs`); whether GitHub would let each open PR merge, as last read
  for the head it names (`DomainState.merge_verdicts`, `model.rs:1523`, table `merge_verdicts`, written only by
  `ObserveMergeVerdict`, `rules.rs:5369`, which refuses a verdict for a PR that is not Open or for another head); the reads of `Merger` and of the watcher. Three writers of
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
| Open | `ObserveMergeVerdict` (system) [same head] | Open; the merge verdict for that head replaces the previous one |
| Closed/Merged, or Open at another head | `ObserveMergeVerdict` | **rejected** `stale_head` |
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

```toml state-machine
id = "M-GH-2"
code = ["crates/domain-rules/src/model.rs::MergeAttemptState", "crates/domain-rules/src/model.rs::MergeAuthorization"]
truth = ["sqlite:merge_authorizations", "sqlite:merge_method"]
states = "crates/domain-rules/src/model.rs::MergeAttemptState"
variants = ["Unclaimed", "Claimed", "Uncertain", "Confirmed", "Failed"]
terminals = ["Confirmed", "Failed"]
initial = ["Unclaimed"]
transitions = ["Unclaimed -> Claimed", "Claimed -> Confirmed", "Claimed -> Failed", "Claimed -> Uncertain", "Uncertain -> Claimed", "Uncertain -> Confirmed", "Uncertain -> Failed"]
```

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

```toml state-machine
id = "M-GH-3"
code = ["crates/engine-github/src/domain_merges.rs::Merger", "crates/engine-github/src/domain_merges.rs::due", "crates/engine-github/src/domain_merges.rs::run", "crates/engine-ports/src/github.rs::MergeState"]
truth = ["memory", "sqlite:branch_update"]
```

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
- **Since G7**: Closed or Open at another head is recorded before `Settled` (F-GH-9); a required review
  (`Status.review_required`) waits with one card (`review`, F-GH-7); GitHub's rate limit (`GhFailure::RateLimited`)
  waits and never counts toward a card or a hand-back (F-GH-8).
- **Concurrency**: one loop only (the `Merger` in `views/actions.rs:3981` is test-only). `still_due` is evaluated with no
  `await` between it and `gh pr merge`; a cancellation that arrives during the `gh` call cannot be avoided.
- **Persistence and restart**: `queued` is lost. A PR already in GitHub's merge queue goes through checks and
  `gh pr merge` again (which GitHub resolves). The retry counters are lost (6 attempts again).

### M-GH-4 · Open PR of a closed task (`OpenPrPending`, table `task_open_prs`)

```toml state-machine
id = "M-GH-4"
code = ["crates/domain-rules/src/model.rs::OpenPrPending", "crates/domain-rules/src/rules/open_pr.rs", "crates/engine-github/src/domain_github.rs::reconcile"]
truth = ["sqlite:task_open_prs"]
states = "crates/domain-rules/src/model.rs::OpenPrPending"
variants = ["Deliver", "Close"]
```

- **Kind**: explicit. **Defined in**: `model.rs:263-272` (`Option<OpenPrPending>` with `Deliver{result}` /
  `Close{comment}`); rules in `rules/open_pr.rs`. Effectors in `domain_github.rs:117-201` (`reconcile`, `deliver_held`,
  `settle_status`) and their in-memory bookkeeping `ObservationFailures` (failed reads, rejected closes with doubled
  wait, cards).
- **States**: None -> `Close` (user cancellation with an open PR, `close_task`) / `Deliver` (closed by the orchestrator or
  Alondra, or `AdoptOpenPr`). `Close` -> None when the PR stops being open (`ObservePrState`, `rules.rs:5138-5142`),
  when another task takes the PR (`RegisterPr`, `rules.rs:4664`) or on reopen (`ReopenTask`, `rules.rs:5300`).
  `Deliver` -> None with `SettleOpenPr` (Closed: `cancel`; Open/Merged: `OpenPrDelivery` delivery card).
- **Effects**: `observer.close(...)` (comment plus close on GitHub) from `reconcile` when the PR is still Open and the
  snapshot says `Close` (`domain_github.rs:131-146`). See F-GH-1. Since G7 a close GitHub accepted is recorded as
  `Closed` at once, which ends the `Close`: Alondra closes a cancelled task's PR once.
- **Persistence and restart**: the pending value lives in SQLite; the failure counters and the doubled wait are lost
  (the attempt is repeated).

### M-GH-5 · PRs seen opening and not yet read (`Task.opened_prs`)

```toml state-machine
id = "M-GH-5"
code = ["crates/domain-rules/src/model.rs::Task"]
truth = ["derived"]
```

- **Kind**: implicit. A set on the task (column `opened_prs` of `task_relations`). `NotePrOpened` adds a PR if it is not
  registered and the task is not terminal; `RecordOpenedPr` removes it and registers/observes it; `RegisterPr` removes
  it. Read on every watcher round (`read_opened`) and on delivery (`catch_up_task`). A PR with no known merge methods
  stays waiting (the log says so; delivery waits).
- **Since #537** (at `68770a298`): the only writer of `NotePrOpened` is `catch_up_task` at a final delivery, which
  notes GitHub's open PR of each open worktree branch of the task (`engine-github/src/domain_github.rs:80-90`);
  `NotePrChanges` (a successful `gh pr close`) re-notes the task's open registered PRs for a fresh read
  (`domain-rules/src/rules.rs:4800-4805`). The providers no longer read `gh pr create` or its output
  (`ObservedFact::PullRequestOpened`/`PullRequestNotNamed` and `runs_gh_pr_create` are gone), and
  `AskToRegisterPr` is kept only so older journals read and does nothing (`:4855-4858`). A one-time upgrade
  (`store-sqlite/src/repository.rs:1640-1652`, marker table `pr_notes_from_github`) drops every note that is not a
  registered PR of its task, freeing deliveries a phantom PR held. States and guards are otherwise unchanged:
  `NotePrOpened` only for an unknown PR of a non-terminal task (`:4806-4814`); `RecordOpenedPr` removes the note
  and registers or observes the PR, refused with `pr_owned_by_other_task` when another task registered it meanwhile
  (`:4815-4854`, owner check `:4823`, see H-GH-6); a final delivery is refused with `pr_being_read` while any note is left (`:4919-4921`).

### M-GH-6 · Stacked PRs (`Task.stack`, table `task_stacks`, `stack_readiness`)

```toml state-machine
id = "M-GH-6"
code = ["crates/domain-rules/src/model.rs::StackDependency", "crates/domain-rules/src/model.rs::StackReadiness"]
truth = ["sqlite:task_stacks", "sqlite:stack_readiness"]
```

- **Kind**: composite. `Task.stack {upstream_task, presentation_id, pr_id, head}` + `StackReadiness{ready}` published by
  the upstream node (`PublishStackReadiness`) + the state of the upstream PR. `stack_ready` (start/due) and
  `stack_ready_for_merge` (claim/merge: upstream PR Merged at that head and equal bases) in `rules.rs:2034-2070`.
- The merger retargets the lower PR to the default branch when the upstream one is merged (`retarget_stack`,
  `retarget_sibling`, `move_onto`, `domain_merges.rs:208-289`) and rereads the state before merging a stacked PR.
- `invalidate_feedback` sets `ready=false`. No new findings: the guards reread GitHub and the domain at every step.

### M-GH-7 · Hourly sweep of PRs without an owner (`open_pr_sweep`)

```toml state-machine
id = "M-GH-7"
code = ["crates/engine-github/src/open_pr_sweep.rs::sweep", "crates/engine-github/src/open_pr_sweep.rs::run"]
truth = ["derived"]
```

- **Kind**: implicit, no state of its own. Every hour (first run after 5 minutes) it looks for the user's open PRs in
  repos of the projects folder. For each PR registered by a terminal task with no pending `Close` and whose branch does
  not belong to a live task (`worktree-<id>`): `ClosePrOfCancelledTask` if the task is cancelled, otherwise
  `AdoptOpenPr` (reopens the task and its delivery) (`open_pr_sweep.rs:68-96`). The domain revalidates both
  (`rules/open_pr.rs:66-103`, `rules.rs:5259-5296`). Since G7 (the user's decision of 2026-11-22) a cancelled task's
  PR whose record Alondra holds closed was reopened by someone: the sweep adopts it (the user gets it as an open PR)
  and `ClosePrOfCancelledTask` refuses it, so it is never closed a second time. `ClosePrOfCancelledTask` is left for
  a record still `Open` with nothing closing it, which only an older build leaves.

### M-GH-8 · Worktree record of a task (`task_worktree_records`, `task_worktrees`, `worktree_storage`)

```toml state-machine
id = "M-GH-8"
code = ["crates/domain-rules/src/model.rs::TaskWorktree", "crates/engine-worktrees/src/task_worktrees.rs"]
truth = ["sqlite:task_worktree_records", "sqlite:task_worktrees", "sqlite:worktree_storage"]
```

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

```toml state-machine
id = "M-GH-9"
code = ["crates/system/src/worktree.rs::discard_worktree", "crates/system/src/worktree.rs::release_task_worktree"]
truth = ["derived"]
```

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

```toml state-machine
id = "M-GH-10"
code = ["crates/engine-worktrees/src/cancelled_worktrees.rs::sweep", "crates/engine-worktrees/src/cancelled_worktrees.rs::release_cancelled"]
truth = ["derived"]
```

- **Kind**: composite. A worktree x the system card `DiscardWorktrees` (question + form; open, answered "delete all" /
  "delete the marked" / "keep", or withdrawn).
- `release_cancelled` (when a cancelled task closes): no uncommitted files -> `discard_worktree` (Removed); with them ->
  `KeptByUser` and one card per task (`:66-107`). `apply_answers` (at the start of **every** closer pass,
  `task_closer.rs:222-224`) deletes with `discard_worktree` (without looking at unsaved work) every path that an answered
  card asked to delete and that is "not in use" (`:121-142`). `sweep` (at daemon start) goes over the cancelled tasks
  (`:146-180`).
- **Terminals**: an answered card is never withdrawn, and compaction keeps Alondra's cards (`compaction.rs:7`), so its
  effect is reapplied forever. See F-GH-2.
- **Final behaviour** (F-GH-2, F-GH-10, F-GH-11): `apply_answers` applies a «delete» answer once and records the path
  `Removed`, also a launch folder the task never recorded (`ReleaseTaskWorktree` with `Removed` on an ended task inserts
  that record); a recorded end stops every later pass. `sweep` never touches a path whose record in the cancelled task
  ended (any end), one that any task's record says the user keeps (`KeptByUser`, `UsersOwn`), or one with commits no
  remote has (`Worktrees::unsaved`): those stay as they are, with no card; uncommitted files still go to the card.

### M-GH-11 · Task close: domain wrap-up + closer bookkeeping

```toml state-machine
id = "M-GH-11"
code = ["crates/domain-rules/src/model.rs::WrapUp", "crates/domain-rules/src/model.rs::NodeEnd"]
truth = ["sqlite:task_wrap_ups", "sqlite:task_closings"]
```

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

```toml state-machine
id = "M-GH-12"
code = ["crates/domain-rules/src/lifecycle.rs::post_merge_owed", "crates/domain/src/lifecycle.rs::PostMergeFact"]
truth = ["sqlite:task_post_merge_work", "sqlite:task_post_merge_turns", "sqlite:post_merge_checks"]
```

- **Kind**: composite. `WrapUp.post_merge_work` + the lifecycle obligation `DrainObligation::PostMerge`
  (`domain/src/lifecycle/commands.rs:384-403`) + the inherited `post_merge_checks` (only read to withdraw its card).
- Declared with `DeclarePostMergeWork{reason}`. It ends **only** with a fact: `DeclarePostMergeWork` without a reason,
  `ConfirmCleanup`, end of session (`RecordNodeEnd Gone`, or the host ending, which gives a resumed session or a card to
  the user, `observations.rs:681-701`), the user's decision on the `PostMergeProblem` card, or (since F-GH-5 was fixed) a
turn that began after the last declaration ending idle after the merge (`PostMergeEnd::TurnEnded`). There is no deadline
  (`task_closer.rs:4-6`, a documented decision). While it is owed, the task is Working ("after merge") and **holds a gate
  slot** (`holds_slot`, `domain/src/lifecycle.rs:1429-1440`). See F-GH-5.

### M-GH-13 · Retention of the files folder (`files_retention.rs`; since PR #503 it also deletes)

```toml state-machine
id = "M-GH-13"
code = ["crates/daemon-support/src/files_retention.rs::sweep"]
truth = ["derived"]
```

- **Kind**: implicit, per folder. `tasks/<id>` (goes when the task is terminal; also in the closer's `finish`),
  `results/<id>` (14 days after close if its card is not in the inbox), `uploads|images|domain-v2|documents/<x>` (30 days
  from last modification if no open card, card of an open task, or document names them), and **any other top-level
  entry** ("without owner", 30 days). Hourly pass in `engine-kernel/src/retention.rs`.
- **Source of ownership**: `References::of` scans only `state.interactions` and `document_revisions`
  (`files_retention.rs:205-220`). It does not look at requests, task entries or the PR image cache. See F-GH-6.
- There is no lock with whoever writes in those folders; it decides from a state snapshot and mtime.

### Findings

#### F-GH-1 · The watcher closes on GitHub the PR of a task the user just reopened (or that another task just took)
**Status**: FIXED in G7 (PR #606). Two parts:
- *The watcher's stale snapshot* no longer happens on `main` at `93dbd23c8`: since #590 (`9c599b4dc`) `reconcile`
  reads `domain.state()` after each PR's GitHub read, so a reopen or a move during the reads of the PRs before it is
  seen. Test `domain_github::tests::the_watcher_reads_the_task_again_before_closing_its_pr` (the minimal sequence
  below: the reopen runs inside the read of the PR read first) passes on `93dbd23c8` unchanged; it stays as a
  regression test. No code change for this part.
- *Closed once, then the user's* (the user's decision of 2026-11-22) did not hold on `93dbd23c8`: the hourly sweep
  (M-GH-7) sent `ClosePrOfCancelledTask` for a cancelled task's PR that Alondra had closed and the user reopened, and
  the watcher closed it again within the next round; and a close GitHub accepted counted only once a read after it
  succeeded, so a failed read left the `Close` pending and a reopen before the next round was closed again. Now:
  `ClosePrOfCancelledTask` refuses (`open_pr_close`, field `pr_id`) a PR whose record is not `Open`
  (`domain-rules/src/rules/open_pr.rs`), the sweep sends `AdoptOpenPr` for it, which delivers it to the user
  (`open_pr_sweep.rs`), and the watcher records a close GitHub accepted as `Closed` at once, without a read after it
  (`domain_github.rs`). Tests: `domain-rules` `open_pr_close_tests::a_cancelled_tasks_pr_alondra_closed_once_and_someone_reopened_goes_to_the_user`;
  `engine-github` `open_pr_sweep::tests::a_cancelled_tasks_pr_reopened_after_alondra_closed_it_goes_to_the_user_and_is_never_closed_again`
  and `domain_github::tests::a_close_github_accepted_is_recorded_even_when_the_read_after_it_fails`. Ablation in the PR.
  The tests that asserted the old re-close (`the_sweep_closes_the_open_pr_of_a_cancelled_task_once_…`, and the
  fixtures that reached a pending close through a reopened PR) were replaced.
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
**Status**: FIXED in group G1 (PR #548). `apply_answers` decides each worktree by the newest card that listed it, applies a «delete» answer only while the task is still `Cancelled` and the path's record is still `KeptByUser` (or the path has no record), and records `Removed`; a cancellation after a reopening asks again under a new card id; `in_use` counts the task itself while it has not ended (`crates/engine-worktrees/src/cancelled_worktrees.rs`, `task_worktrees.rs`). Test: `an_answered_delete_card_never_touches_the_worktree_of_the_task_reopened_after_it` (`//crates/engine-sdk:engine_sdk_engine_test`), which also cancels the reopened task again. Ablation: with the module and `in_use` as on `main` the test fails; restored, it passes.
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
**Status**: FIXED in group G1 (PR #548). The startup sweep filters the launch folder of `task_worktrees` by the end of its record like any other path, and `in_use` counts as owner any other task that holds the path open, also while it is ending (`cancelled_worktrees.rs`, `task_worktrees.rs`). Test: `the_startup_sweep_leaves_the_worktree_a_cancelled_task_handed_to_a_task_still_closing` (`//crates/engine-sdk:engine_sdk_engine_test`); `a_folder_an_open_continuation_launches_in_is_never_swept_even_when_it_looks_empty` now expects the folder to stay while the cancelled continuation still holds it and to go once its close settles it. A card already pending about a folder still covers it whichever task listed it, so two tasks sharing one folder get one card. Ablation: with the sweep and `in_use` as on `main` the worktree is deleted; restored, it stays.
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
**Status**: FIXED on `main` by #532 (commit `ee9bf0dc8`), merged while this phase was being built: the branch goes only when the default branch holds it or, once the task's pull request merged, its tip is on the branch's own copy on a remote, read with `git fetch` then (`branch_is_expendable`, `crates/system/src/worktree.rs`), so a repository without a remote keeps it. Group G1 (PR #548) adopts that fix and adds what it lacked: the `Worktrees` contract case `finished_branches` (repository without a remote), passed by the Mac's git and by the double, and the double now follows the same rule (it still deleted the branch on a merged pull request alone). Engine test: `closing_a_task_whose_pr_merged_in_another_repository_keeps_the_branch_of_its_repository_without_a_remote` (`//crates/engine-sdk:engine_sdk_engine_test`). Ablation: with the double deleting on a merged pull request alone, the contract and the engine test fail; restored, both pass.
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
**Status: FIXED** in #553. On `main` an idle rest after the merge woke the node and then opened a case for the
orchestrator, but nothing ended the work: the documented rule (a turn after the merge that ends with nothing running ends
the work unless the node declared it again) was lost with the `claude --bg` node machine in #173, which left
`PostMergeWork.turn_since` never set and `PostMergeEnd::TurnEnded` never recorded. Now `record_runtime_facts` marks
`turn_since` when a turn starts (`TurnStarted`, `TurnUnprompted`) and a declaration resets it; `finish_observed_turn` ends
the work with `PostMergeEnd::TurnEnded` when the turn ended empty, `turn_since` is set, the PR merged
(`work_after_merge`), the activity is `Resting` (no own work), no input waits, no card holds it and the work waits for no
task. Tests: `answered_card_tests::after_the_merge_a_turn_that_ends_idle_without_declaring_its_work_again_ends_that_work`
(without the fix the work stays owed) and `…_a_turn_that_ends_behind_work_of_its_own_leaves_that_work_owed`.
**Class**: CONFIRMED · **Severity**: S2 · **Probability**: medium-low · **Overlap**: the stalled-node recovery work (phases 2-3: contradictions without a timer, `WakeUntaken`) and the node anomaly detection work (signal "turn that ends with no delivery or own work"). Neither names the post-merge case; the evidence is new for them.
- **Invalid state/transition and broken invariant**: a `PostMerge` obligation with nothing that will end it. It breaks "only a
  finished or cancelled state owes nothing; every step has an owner that gives it" (`docs/architecture/task-states.md`,
  "Who owes the next step") and the product rule that a task only waits for capacity or for the user.
- **Evidence**:
  1. Only a fact ends post-merge work (domain-rules `lifecycle.rs:2449-2472`; `task_closer.rs:4-6`).
  2. The node's turn ends idle and there is no deadline for this in `obligation_policy` (only HandInput, Wake, Answer; since #516 only HandInput and Wake, `obligation_policy.rs:13-27` at `68770a298`: the step still holds).
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
**Status**: FIXED in group G1 (PR #548), with the user's decision on `files/`. Retention indexes the lifecycle journal and pending inputs of open tasks and the bodies of open requests, so a message's upload stays while its task is open; only Alondra's own folders (`tasks`, `results`, `uploads`, `images`, `domain-v2`, `documents`) are ever deleted from, and anything else is listed and never removed (`crates/daemon-support/src/files_retention.rs`). Tests: `an_upload_a_message_of_an_open_task_names_stays_after_thirty_days`, `a_file_outside_alondras_folders_is_listed_and_never_deleted`. Ablation: both fail with the old rules; restored, both pass.
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
**Status**: FIXED in G7 (PR #606). `Status` carries `review_required` (GitHub's `reviewDecision` is
`REVIEW_REQUIRED`, read in the same `gh pr view`, `github-gh/src/merges.rs`). The merger checks it before claiming
and merging when the PR is not `CLEAN`, and again in `merge_failed`: it raises one «Get PR #n approved on GitHub»
card and waits, with the acceptance valid and nothing handed back; the card is withdrawn once the review is in
(`domain_merges.rs`, `ask_review`/`clear_review`). `CHANGES_REQUESTED` still goes back to the node as before. Test:
`domain_merges::tests::a_merge_github_holds_for_a_required_review_waits_for_it_with_the_acceptance_intact`.
Ablation in the PR.
Merge-button task: on top of that, GitHub's verdict on merging (`MergeVerdict`, read through `gh` and recorded by `ObserveMergeVerdict`) tells the delivery card why the merge waits: the card carries the merge button disabled, with that reason under it, on iPhone, Mac and the terminal (`CardAction::offered` keeps a disabled action only when it says why; `views/src/card.rs` `blocked_merge`), and it turns usable on its own when GitHub allows the merge. Tests: `a_merge_github_refuses_for_a_required_review_waits_and_keeps_the_acceptance`, `a_merge_github_does_not_allow_yet_shows_the_merge_disabled_and_says_why_on_every_client`, the goldens `card-merge-blocked*.json`, the Swift snapshots `test_aMergeGitHubDoesNotAllowYetIsDisabledAndSaysWhy` and scenario `merge-waits-for-the-review-github-requires`.
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
**Status**: FIXED in G7 (PR #606). `GhFailure::RateLimited` is GitHub's spent limit, told by typed signals
only, never by `gh`'s words: an `HttpAnswer` that `rate_limited()` (429, or 403 with no requests left or a
`retry-after`), or a GraphQL answer with an error of type `RATE_LIMITED` (`graphql_rate_limited`). `gh pr view`
only prints text, so a failed read of the watcher or of the merger asks the same address through
`gh api graphql --include` once, the call the watcher already made to tell a missing PR (`view_failure`,
`limit_failure`, `github-gh/src/cli.rs`, `merges.rs`). It counts toward no card: the watcher logs it and reads again next
round (reads and closes); the merger's `troubled` ignores it, and a merge, a `ready` or a read after a failed merge
that hit it waits for the next poll instead of a retry that ends in a hand-back. Showing «waiting for the GitHub
API» on the task is not part of this fix. Tests: `domain_github::tests::githubs_api_limit_never_raises_a_card_in_the_watcher`,
`domain_merges::tests::githubs_api_limit_never_raises_a_merge_card_or_hands_the_delivery_back`, and the
classification in `github-gh` `cli_tests::githubs_rate_limit_is_told_apart_from_other_failures`. Ablation in the PR.
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
**Status**: FIXED in G7 (PR #606). `step` records what it read (`observe_status`) when GitHub reports the PR
closed or open at another head, as it already did for a merge, so the PR stops being due at once. Test:
`domain_merges::tests::a_pr_closed_or_pushed_outside_alondra_is_recorded_and_read_once` (10 merger passes, one
GitHub read, for both cases). Ablation in the PR.
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

#### F-GH-10 · A «delete» answer about a launch folder the domain never recorded is applied again on every closer pass
**Class**: CONFIRMED · **Severity**: S1 · **Probability**: low · **Overlap**: F-GH-2 (same card, the path without a record).
**Status**: FIXED (review of PR #548). After deleting, `apply_answers` records the path `Removed` whether or not the task had a record of it: `ReleaseTaskWorktree { end: Removed }` on an ended task now inserts a `Removed` record (`origin: Task`) for a path it never recorded, and any other end is still `not_found` (`crates/domain-rules/src/rules.rs`, `crates/engine-worktrees/src/cancelled_worktrees.rs`). Tests: `a_delete_answer_about_a_folder_the_domain_never_recorded_is_applied_once` (`//crates/engine-sdk:engine_sdk_engine_test`), `an_ended_task_records_as_removed_a_launch_folder_it_never_recorded` (`//crates/domain-rules:domain_rules_test`). Ablation: with `apply_answers` releasing only recorded paths (as on `main`), the folder made there after the answer is deleted again and the engine test fails; restored, it passes.
- **Evidence**: the folder the sweep or a cancellation lists can be only the task's launch folder (`task_worktrees`), with
  no record in `task.worktrees`. `apply_answers` lets `recorded == None` through and, after `discard_worktree`, recorded the
  end only `if recorded.is_some()` (`cancelled_worktrees.rs`, on `main` at `8d46b4783`). Nothing marks the answer applied,
  so every closer pass (every 30 s or on every commit) deletes whatever is at that path again.
- **Minimal sequence**: cancelled task with an unrecorded launch folder P holding a file; the startup card; «Delete»; P
  deleted; a worktree is made again at P with a new file; next closer pass: P deleted again.
- **Deterministic test**: engine with the `Worktrees` double; `make_worktree` recreates P; three closer passes and a sweep.

#### F-GH-11 · The startup sweep deletes a folder another finished task kept with «Leave», and commits not pushed
**Class**: CONFIRMED · **Severity**: S1 · **Probability**: low · **Overlap**: F-GH-3 (that fix read only the cancelled task's own record).
**Status**: FIXED (review of PR #548). `sweep` skips a path that any task's record ended `KeptByUser` or `UsersOwn`, a path whose own record ended at all, and a path whose worktree holds commits no remote has (`settle_path` with `keep_unpushed`, through `Worktrees::unsaved`); such a folder is left as it is, with no card (`crates/engine-worktrees/src/cancelled_worktrees.rs`). The cancellation itself (`release_cancelled`) is unchanged. Tests: `the_startup_sweep_never_deletes_a_folder_a_finished_task_kept_with_commits_not_pushed`, `the_startup_sweep_leaves_a_cancelled_worktree_whose_commits_no_remote_has` (`//crates/engine-sdk:engine_sdk_engine_test`). Ablation: with the sweep as on `main` both fail (the folder is deleted); restored, both pass.
- **Evidence**: `release_cancelled` hands an unrecorded launch folder to the task working in it (`heir`) but records the end
  only for recorded paths, so the cancelled task T1 keeps a `task_worktrees` row P with no record. T2 then finishes, its
  close stops on commits not pushed and the user answers «Leave it open»: T2's record of P ends `KeptByUser`. At the next
  daemon start, T1's `sweep` filtered P only by T1's records (none), `in_use` ignores ended records, no discard card asked
  about P, and `settle_path` counts only uncommitted files (0), so P was deleted.
- **Impact**: the folder the user said to leave disappears with its ignored files; commits on a detached head or another
  branch are left unreachable.
- **Deterministic test**: engine with the `Worktrees` double: T1 cancelled with launch folder P, T2 recorded in P,
  `local_commit(P)`, three failed closes, «Leave», then `sweep`.

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

#### H-GH-6 · A branch PR noted for a task and registered meanwhile by another live task blocks that task's final delivery for good
Added at `68770a298` while re-checking #537 (the mechanism predates it; #537 made the branch read the only way a PR is
noted). Owner: the stable pull request identity work (referred table); no fix group here.
What is claimed: task A presents a final delivery; `catch_up_task` reads GitHub's open PR P of one of A's worktree
branches and, since nobody registered P, notes it (`NotePrOpened`, `engine-github/src/domain_github.rs:84-90`;
`domain-rules/src/rules.rs:4806-4814`). If task B registers P before A's note is read, every later
`RecordOpenedPr { A, P }` is refused with `pr_owned_by_other_task` (`rules.rs:4823`); the refusal rolls back the
removal of the note, so A keeps P in `opened_prs`. `read_opened` retries it every watcher round and only logs
(`domain_github.rs:59-66`), and A's every presentation is refused with `pr_being_read` ("send the same result again in
a moment", `rules.rs:4919-4921`). A's node cannot clear it either: its `register_pr` of P is refused the same way while
B is live (`:4729-4735`), and `RecordOpenedPr` has no terminal-owner branch, so the note outlives B too unless A's node
registers P after B ended (which moves it). Severity if confirmed: S2 (A cannot deliver; cancelling is the way out).
Missing to confirm: a real flow in which two live tasks work on the same branch, so that B registers the PR of A's
worktree branch between A's note and its read (a handed-over worktree leaves A's open worktrees, `open_worktrees`).
Proposed test: pure domain, two tasks, `NotePrOpened{A,P}`, `RegisterPr{B,P}`, `RecordOpenedPr{A,P}` refused, then A's
`PresentResult{is_final}` refused with `pr_being_read`; expected: the note dropped when another live task owns the PR.

## Orchestrator and schedules (`ORCH`)

Scope: `crates/engine-orchestrator` (executor, recovery, watchdog, inbox, providers),
`crates/daemon-engine/src/domain_orchestrator_tools.rs`, `crates/core/src/orchestrator.rs`, `orchestrator.json`,
the `cancel|acknowledge|retry` subcommands of the orchestrator CLI, the `Answer` deadline
(`daemon-engine/src/obligation_watch.rs` + `orchestrator_inbox::overdue`), and schedules
(`domain-rules/src/schedule.rs`, `engine-sdk/src/domain_schedule_runner.rs`, `domain_schedule_api.rs`, tables
`schedules`, `schedule_runs`, `schedule_failed_launches`, `auto_launches`, `schedule_paid_accounts`). Audited at commit
`1567967b9`, by reading only. Re-checked for #516 at `68770a298`: the `Answer` deadline and `orchestrator_inbox::overdue`
no longer exist (M-ORCH-5, M-ORCH-6); `orchestrator_inbox.rs` was read whole at that commit.

### M-ORCH-1 · Orchestrator execution (`orchestrator.json`, `Runtime`)

```toml state-machine
id = "M-ORCH-1"
code = ["crates/engine-orchestrator/src/domain_orchestrator.rs::Runtime", "crates/engine-orchestrator/src/domain_orchestrator.rs::Failure", "crates/engine-orchestrator/src/domain_orchestrator.rs::Run", "crates/engine-orchestrator/src/domain_orchestrator.rs::handled", "crates/core/src/model.rs::OrchestratorState"]
truth = ["file:orchestrator.json", "sqlite:orchestrator_session"]
```

- **Kind**: implicit, composite. **Defined in**: `domain_orchestrator.rs:37-71` (`Runtime`), `:82-95` (`Failure`),
  `:97-104` (`Run`).
- **Source of truth**: the file `HOME/orchestrator.json` (written as `.next` + rename), one writer at a time under
  `Orchestrator.record` since G3 (F-ORCH-7). **Copies and derivatives**: `DomainRuntime.orchestrator_rests` (atomic, reloaded on every tick `:457`),
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
  - Running -> `Denied` with no Alondra call, no admitted operation and no turn that acted (`refused_before_any_work`) -> the
    same Backoff as `NotStarted`, no card; a Codex account that refused its native policy becomes `PolicyRefused`
    (`RecordAccountPolicyRefused`), a state apart from exhausted that no time clears, so selection skips it. `resume_stopped` treats a remembered `Denied` with no calls the same way.
  - Running -> `Interrupted`/`Denied`/`Cancelled{effects}` with doubtful effects -> Ambiguous + card (`:764-773`).
  - Running -> `Interrupted` with confirmed effects -> handoff + Backoff (`:658-665`).
  - Running -> `Limited` -> Idle (+handoff).
  - Ambiguous -> acknowledge -> Idle with `pending_resolutions` (`:313-337`); -> retry -> Idle, messages `retryable`
    (`:340-362`).
  - Daemon start: `startup_recovery` -> `repair_incomplete` (`recovery.rs:4-345`); see F-ORCH-1.
  - Since #516 (at `68770a298`, `handled` `domain_orchestrator.rs:676-718`): after `finish_batch`,
    `settle_orchestrator_batch(Handed)`, the announcements and `rested`, it reads whether a message already waits
    (`:705`) **before** adding its own notices (`hand_replans`, `:706`; `go_on_after_the_task_limit`, `:707`,
    M-ORCH-13), and settles the inbox (`:711`) only when nothing waited. An idle `tick` settles too, but only with no
    pending message or resolution, no usage pause, a selectable account and `last_error` None (`:470-497`).
- **Rejected**: `acknowledge/retry` of another run -> `bail!` -> HTTP 409 (`:1001-1017`); `decide` requires `last_error`
  except for `Cancel`.
- **Terminals**: none; it is a cycle.
- **Invariants**: assumed, not enforced: `failure.is_some() => last_error.is_some()`; Ambiguous => an open card.
  Representable but meaningless: `active_run=None AND failure=Denied`.
- **Persistence and restart**: the file survives; `runs`, `stop`, `host`, the watchdog and the deadline clock are lost.
  An empty `runs` after a restart is exactly what makes a batch "cold" (F-ORCH-1).
- **Concurrency**: one loop (`run`) with `Ownership` (a file lock between processes) and `HostGuard` (one host). The
  HTTP server's `cancel` and the closure `mark_spawned` read, change and write under `Orchestrator.record`, and every
  other write takes the same lock (F-ORCH-7, fixed). The tick's own copy is still held across a batch; it is the
  only writer while the batch runs besides those two.

### M-ORCH-2 · Delivery of each message to the orchestrator (outbox)

```toml state-machine
id = "M-ORCH-2"
code = ["crates/store-sqlite/src/lib.rs::finish_message", "crates/store-sqlite/src/batch_recovery.rs::settle_orchestrator_batch", "crates/engine-ports/src/journal.rs::resolve_uncertain", "crates/core/src/delivery.rs::State"]
truth = ["sqlite:domain_outbox", "sqlite:delivery_attempts", "sqlite:orchestrator_deliveries"]
```

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

```toml state-machine
id = "M-ORCH-3"
code = ["crates/store-sqlite/src/batch_effects.rs", "crates/store-sqlite/src/batch_recovery.rs::unfinished_orchestrator_batches"]
truth = ["sqlite:orchestrator_batch_runs", "sqlite:orchestrator_batch_decisions", "sqlite:orchestrator_tool_operations", "sqlite:orchestrator_batch_effects", "sqlite:orchestrator_batch_reports", "sqlite:orchestrator_batch"]
```

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

```toml state-machine
id = "M-ORCH-4"
code = ["crates/engine-orchestrator/src/engine.rs::Orchestrator", "crates/engine-orchestrator/src/domain_orchestrator.rs::Run"]
truth = ["memory"]
```

- **Kind**: implicit. `Orchestrator { host, stop, runs }` (`engine.rs:16-26`), `Watchdog { progress, since, fired,
  restart_pending }` (`watchdog.rs:49-58`), `DomainRuntime.orchestrator_progress`
  (`engine-kernel/src/domain_runtime.rs:126-132`), `orchestrator_rests`. All of it is lost on restart. The watchdog fires
  after 5 min without progress while requests without a task exist (`watchdog.rs:80-99`, `:148-185`) and restarts the
  executor (`:191-234`).

### M-ORCH-5 · Settling the inbox (`orchestrator_inbox::steps/settle`)

```toml state-machine
id = "M-ORCH-5"
code = ["crates/engine-orchestrator/src/orchestrator_inbox.rs"]
truth = ["derived"]
```

- **Kind**: implicit (numbered letters keyed by id). **Defined in**: `orchestrator_inbox.rs:52-72`, `:207-252`, `:265-275`.
- **Events and transitions, per request without a task**: message -> `Close`; request -> `UnsplitNotice-1` (to the
  orchestrator) -> `UnsplitProblem-n` (to the user) -> answer `close` -> `Close(withdrawn)`; `retry` -> a new round.
  Suggestion -> `UnsettledNotice` once.
- **Guard**: only when nothing is pending for the orchestrator and (in `tick`) `last_error` is None
  (`domain_orchestrator.rs:488-493`).
- **Persistence and restart**: idempotent: the letter id is derived; `carry_out` refuses to create an existing letter
  (`:443-447`).
- **Since #516** (at `68770a298`; `orchestrator_inbox.rs` whole, 1-512 without tests): one rule settles everything the
  orchestrator owes, read from state, with no clock (module doc `:1-23`).
  - **Items** (`steps` `:65-81`, `owed` `:86-112`): per open request without a task and without an orchestrator question
    to the user about it (`asked_about`, `:183-190`): a message closes (`Step::Close`), a user's request goes
    `UnsplitNotice-n` → `UnsplitProblem-n` (`unsplit`, `:232-273`); per live task: a node's suggestion
    (`unsettled`, `:298-311`), a stopped node's case (`stalled`, `:319-335`), a node's question (`unanswered`,
    `:356-369`) and a held plan review not asked about (`unreviewed`, `:373-381`); and the retired problems of older
    releases are withdrawn (`:67-69`, `:139-142`).
  - **States per item**: no reminder → reminder `n` sent (an inbox card to the orchestrator, id `<prefix><subject>-<n>`,
    `inbox_card` `:228-230`) → reminder read (`reminded`, `:205-215`: a card whose id has the numbered form and whose
    message is `handed` in the outbox, `orchestrator_notice_read`, `store-sqlite/src/lib.rs:1554-1557`) → Alondra's
    outcome: `Raise` of the user's problem for a request, `Discard` of a suggestion, `SettleStall(Instruct |
    Unsettled)` of a case (`Instruct` while the task has attempts left and is not delivered), `Withdraw` of a question,
    `ReleaseUnreviewed` (`SettlePlanReview(Unchanged)`) for each held task. A reminder never read (its message
    discarded at 24 h, its batch stopped, discarded or given up) does not count: the next settle sends reminder `n+1`.
    Reminders an older daemon wrote under an id with no number never count.
  - **When it runs**: only at the end of a handled turn with nothing waiting (M-ORCH-1, `domain_orchestrator.rs:705-711`)
    and in an idle `tick` with nothing pending, no usage pause, an account to select and `last_error` None
    (`:491-497`). Never from a clock: `settle_overdue` now only steps what Alondra owes the nodes (`:428-430`).
  - **Domain side**: `SettleSuggestion` accepts a discard from System (`domain-rules/src/rules.rs:897-906`,
    responder `alondra`); `SettleStall(Unsettled)` of a task with an open PR and an unanswered user message now
    delivers the PR with `UNSETTLED_UNANSWERED` instead of being refused (`rules/stall.rs:182-187`).
  - **Consequence**: with no handled turn, nothing the orchestrator owes moves (see F-ORCH-3 and F-ORCH-1).

### M-ORCH-6 · `Answer` deadline (what the orchestrator owes a task) — removed by #516

```toml state-machine
id = "M-ORCH-6"
code = []
removed = true
```

Removed at `68770a298`: `ANSWER`, `Action::Answer`, `settle_answers`, the `OrchestratorAnswers` port and
`orchestrator_inbox::overdue` no longer exist; what follows describes the audited commit. What replaced it is the
state rule of M-ORCH-5.
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

```toml state-machine
id = "M-ORCH-7"
code = ["crates/engine-orchestrator/src/orchestrator_inbox.rs::hand_replans"]
truth = ["derived"]
```

- **Kind**: implicit. **Defined in**: `orchestrator_inbox.rs:97-118`, `:286-301`, `:334-360`; the obligation in
  `domain-rules/src/ownership.rs:337-355`.
- A task that waits for a cancelled one, or whose launch plan was refused: at every "rest" (`rests`) of the orchestrator a
  letter `dependency-cancelled-<t>--<d>-<rest>` / `plan-refused-<card>-<rest>` is created, at most `REPLAN_REMINDERS` (3)
per subject; a batch that carried only such letters (`Run.replans_only`) is no rest. No deadline. See F-ORCH-2 (fixed).
- **Since #516** (at `68770a298`; `replans` `orchestrator_inbox.rs:118-126`, `hand_replans` `:130-136`,
  `plan_refused` `:338-352`, `dependency_cancelled` `:386-415`): unchanged, one letter per rest. What changed is that a
  re-plan letter no longer keeps the rest of the inbox from being settled: `handled` reads what was waiting before it
  hands them (`domain_orchestrator.rs:705-706`). The loop of F-ORCH-2 was closed in phase 5 (above).

### M-ORCH-13 · New Tasks per turn of the orchestrator (`TURN_TASK_LIMIT`, added by #516)

```toml state-machine
id = "M-ORCH-13"
code = ["crates/domain-rules/src/orchestrator_batch.rs::TURN_TASK_LIMIT", "crates/store-sqlite/src/batch_effects.rs::within_task_limit", "crates/engine-orchestrator/src/domain_orchestrator.rs::go_on_after_the_task_limit"]
truth = ["derived"]
```

- **Kind**: implicit, counted from the store. **Defined in** (at `68770a298`): `TURN_TASK_LIMIT` = 10 and the code
  `turn_task_limit` (`domain-rules/src/orchestrator_batch.rs:12-13`); `tasks_created_in` and `within_task_limit`
  (`store-sqlite/src/batch_effects.rs:75-104`), checked before each batch command (`:68`);
  `go_on_after_the_task_limit` (`engine-orchestrator/src/domain_orchestrator.rs:722-735`).
- **States per batch**: under the limit → at the limit (10 tasks whose `created_revision` is the revision of a
  committed `create_task` or `settle_suggestion` effect of that run). At the limit, a `CreateTask` or a
  `SettleSuggestion { NewTask }` is refused with `turn_task_limit` before any effect, unless the same command id and
  epoch were already committed (a replay of an interrupted turn, answered with its receipt). The check and the commit
  run under the same store lock, so two creates cannot both pass at 9.
- **After the turn**: if it created 10, Alondra tells the orchestrator to go on (`turn-task-limit-<run>`), a message
  that starts the next turn at once; that notice is added after `handled` read what was waiting, so it does not hold
  back the settle of the same `handled` (see H-ORCH-4).

### M-ORCH-8 · Retention of announcements (rule 10)

```toml state-machine
id = "M-ORCH-8"
code = ["crates/engine-orchestrator/src/domain_orchestrator.rs::handled", "crates/engine-kernel/src/domain_runtime.rs::announce_request_tasks"]
truth = ["derived"]
```

- **Kind**: implicit. `announce_request_tasks` runs only from `handled` (`domain_orchestrator.rs:681`); notices of the
  batch's cards are held while `orchestrator_batch_runs.ended=0` (`lib.rs:1576`, filter in
  `daemon-server/src/domain_delivery.rs:43-44`).

### M-ORCH-9 · Schedule (`Schedule`)

```toml state-machine
id = "M-ORCH-9"
code = ["crates/domain-rules/src/schedule.rs::Schedule"]
truth = ["sqlite:schedules", "sqlite:schedule_paid_accounts", "sqlite:schedule_request_tools"]
```

- **Kind**: explicit (a validated struct). **Defined in**: `domain-rules/src/schedule.rs:25-45`, `:186-259`.
- **Source of truth**: table `schedules` (+ `schedule_paid_accounts` for the paid-account grant,
  `store-sqlite/src/provider_accounts.rs:15-24`).
- **States**: enabled with `next_run`; enabled without `next_run` (a consumed Once); disabled; deleted (tombstone).
- **Invariants**: enforced: `!enabled => next_run=None`; deleted => disabled; valid interval and anchor.

### M-ORCH-10 · Scheduled run (`ScheduleRun` x `ScheduleLaunch`)

```toml state-machine
id = "M-ORCH-10"
code = ["crates/domain-rules/src/schedule.rs::ScheduleRun", "crates/domain-rules/src/schedule.rs::ScheduleLaunch", "crates/domain-rules/src/schedule.rs::claim_launch", "crates/engine-sdk/src/domain_schedule_runner.rs", "crates/domain-rules/src/schedule.rs::ScheduleLaunchState", "crates/domain-rules/src/schedule.rs::ScheduleRunState"]
truth = ["derived", "sqlite:schedule_failed_launches", "sqlite:schedule_runs"]
```

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

```toml state-machine
id = "M-ORCH-11"
code = ["crates/engine-sdk/src/auto_launch.rs::settle", "crates/engine-ports/src/journal.rs::claim_auto_launch", "crates/engine-ports/src/journal.rs::AutoLaunchState"]
truth = ["sqlite:auto_launches"]
```

- **Kind**: implicit. `launching` -> `launched` | `blocked(reason)`; deleted to retry (`engine-sdk/src/auto_launch.rs:62-77,102-176`,
  `store-sqlite/src/lib.rs:255,1327-1431`). It belongs mostly to admission; its race with `create_task` is gone since
  the task carries its priority from its creation (F-ORCH-8, fixed).

### M-ORCH-12 · Legacy reducer `core::orchestrator`

```toml state-machine
id = "M-ORCH-12"
code = ["crates/core/src/orchestrator.rs::Phase", "crates/core/src/orchestrator.rs::validate", "crates/core/src/orchestrator.rs::apply"]
truth = ["derived"]
states = "crates/core/src/orchestrator.rs::Phase"
variants = ["Idle", "Starting", "Running", "Compacting", "Failed"]
```

- **Kind**: explicit. `Phase { Idle, Starting, Running, Compacting, Failed }` with `validate/apply`
  (`core/src/orchestrator.rs:13,80-200`). The current executor does not use it (only `NODE_ID`); it remains as an old view
  event (`core/src/event.rs:98`). It is a duplicate machine that no longer describes the real one: any read of
  `view.orchestrator` contradicts `orchestrator.json`. No reachable finding of its own.

### Findings

#### F-ORCH-1 · A daemon restart in the middle of a batch stops the orchestrator until the user answers a card
**Status**: FIXED in `2fc1e9349` (G3, #520). A batch cut by the daemon's own stop (`note_shutdown` on SIGTERM, a new release or a handover writes `cut_by_shutdown`) resumes without a card through `startup_recovery`; a batch cut without that mark keeps the conservative card. Tests: `engine-orchestrator` `recovery_decisions::a_batch_cut_by_the_daemons_stop_*` and `…::a_batch_cut_without_the_daemons_stop_still_asks_before_anything_runs_again`.
**Class**: CONFIRMED · **Severity**: S2 · **Probability**: medium: batches are frequent (every request, reminder or case) and the daemon restarts on every version · **Overlap**: the provider adapters work (phase 6) moves the orchestrator onto the adapters (it may touch this recovery): the evidence is NEW. Not in open issues. `git log` of the file shows no fix after d3dcbf2c9, which introduced `cold_batch`.
**Status**: still holds at `68770a298`: #516 does not touch recovery. Its impact changed: the last sentence of
"Impact" below ("`Answer` deadlines do keep running") is no longer true. With the `Answer` deadline gone (M-ORCH-6),
while the orchestrator stays Ambiguous (`tick` leaves at `active_run.is_some()`, `domain_orchestrator.rs:466-468`)
nothing settles what it owes the nodes either: their questions, suggestions, stopped-node cases and held plan reviews
wait for the user's answer to the batch card. That answer has a visible owner, so no separate finding; it is the
counterpart of H-ORCH-3, which no longer happens.
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
**Status: FIXED** in #553, with the default decision. A batch whose messages were all re-plan reminders
(`only_replan_reminders`, stored as `Run.replans_only`) does not count as a rest, so a reminder no longer starts the batch
that hands the next one; and each subject (task and cancelled dependency, or refusal card) is handed at most
`REPLAN_REMINDERS` (3) times. Past it the task keeps its factual state («“X” was cancelled · the orchestrator decides what
happens to it») and no batch is spent on it until what it waits on changes. Tests:
`orchestrator_inbox::tests::a_replan_the_orchestrator_never_decides_starts_a_bounded_number_of_batches` and
`only_a_batch_of_nothing_but_replan_reminders_is_no_rest`; `a_wait_for_a_cancelled_task_is_handed_back_…` and the engine
test of refused plans now stop at the third reminder.
**Class**: CONFIRMED · **Severity**: S2 (duplicated work) · **Probability**: low-medium: it depends on the model not complying or its action being rejected (for example a plan that `update_launch_plan` keeps rejecting) · **Overlap**: the note in `docs/architecture/task-states.md` ("Waiting for a cancelled task advances only when the orchestrator is idle and nothing fails; a task will give it an owner and a deadline") addresses the opposite problem; this loop is NEW evidence.
**Status**: still holds at `68770a298`. #516 kept `replans` per rest number (`orchestrator_inbox.rs:118-126`,
`:340`, `:395`) and `hand_replans` after every turn (`domain_orchestrator.rs:706`, `:771`): every rest still creates
a new letter, which is a pending message that starts another turn. #516 only stopped those letters from blocking the
settle of everything else (`:705`). Refreshed refs: `rested` `:763-766`; the idle `tick` hands them too (`:465`, `:493`).
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
**Status**: FIXED in `2fc1e9349` (G3, #520). With no message left to retry, `tick` clears the launch failure and settles; the watchdog is suspended while a start's backoff waits. Tests: `tests::a_request_whose_message_expired_during_failed_starts_goes_back_to_the_orchestrator`, `watchdog::tests::the_watchdog_waits_out_a_failed_starts_backoff`.
**Class**: CONFIRMED · **Severity**: S2 · **Probability**: low (24 consecutive hours of launch failures) · **Overlap**: none known.
**Status**: still holds at `68770a298`, and #516 widened its impact (new evidence, same root cause and fix group G3).
In the "failed with no exit" state `tick` discards the expired messages (`domain_orchestrator.rs:479-483`), finds
nothing pending, hands the re-plans and skips `settle` because `last_error` is set (`:491-497`); no batch runs, so
`handled` never settles either (`:705-711`). Before #516 the `Answer` deadline of `obligation_watch` still settled
what the orchestrator owed the nodes after 10 + 10 minutes, busy or failing. Since #516 nothing does: the policy
enforces only `HandInput` and `Wake` (`domain-rules/src/obligation_policy.rs:24-26`). So a node's question to the
orchestrator is never reminded nor withdrawn (its node stands still), a stopped node's case is never settled, a
suggestion never discarded and a held plan review never released, for as long as the state lasts; the only way out is
the user cancelling each task. The same holds during the 24 hours of failures before it, only bounded by the
provider coming back. Test addition: in the test below, also create a node's question to the orchestrator before the
messages expire, and assert that after `tick` it is reminded (an `UnansweredNotice` card) or withdrawn; today neither
happens. The model test cannot show it: its orchestrator always ends a handled turn (M-LIFE-14).
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
**Status**: FIXED in `2fc1e9349` (G3, #520). `recover_uncertain` records the launch Handed to the node a task already has; `answer_failed_launches` leaves such a task alone. Tests: `engine-sdk` `domain_schedule_runner::tests::a_launch_cut_after_its_node_was_admitted_is_handed_to_that_node_and_answering_never_cancels_it`, `…::answering_a_failed_launch_card_leaves_a_task_that_got_its_node_running`.
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
**Status**: FIXED in `2fc1e9349` (G3, #520). Unreadable settings leave the run Pending with the refusal `launch_failed` and its reason in the detail; `run_cycle` isolates each run's failure and only storage errors stop the worker. Test: `engine-sdk` `domain_schedule_runner::tests::unreadable_settings_leave_a_pending_run_waiting_with_its_reason_and_the_runner_going_on`.
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
**Status**: FIXED in `2fc1e9349` (G3, #520). The six call sites count progress with `note_orchestrator_progress`; the read is `#[must_use]`. Test: `engine-kernel` `batch_operations::tests::admitting_and_finishing_a_tool_operation_each_count_as_orchestrator_progress`.
**Class**: CONFIRMED · **Severity**: S3 · **Probability**: low · **Overlap**: none.
**Status**: still holds at `68770a298`: `handled` still calls the read `self.domain.orchestrator_progress()` as a
statement (`domain_orchestrator.rs:701`); #516 moved the lines around it, not the call.
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
**Status**: FIXED in `2fc1e9349` (G3, #520). One writer at a time through `Orchestrator.record`; the cancel and the host's start are locked read-change-write. Test: `tests::the_hosts_start_holds_the_record_from_its_read_to_its_write_so_a_cancel_cannot_land_in_between` (a write hook checks the lock is held at the write, so a cancel waits; no threads or sleeps).
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
**Status: FIXED** in #553. `Command::CreateTask` has `priority` (`TaskPriority`, `#[serde(default)]` so commands
recorded before read back as normal), stored on the task in the creating commit; the MCP `create_task` tool takes it and
the orchestrator skill says to pass it there; `set_priority` only changes it later. Test:
`auto_launch::tests::a_task_created_with_a_high_priority_takes_the_next_free_slot_before_an_older_one` (without the fix
the older normal task takes the slot).
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
**Status**: no longer reachable since #516 (commit `e10b2839e`): there is no `Answer` deadline any more
(`domain-rules/src/obligation_policy.rs:13-27` at `68770a298`; `obligation_watch` has no `Action::Answer`), and Alondra
settles what the orchestrator owes only after a turn of it ends handled, from reminders a handled batch was handed
(M-ORCH-5). The opposite now holds: while the orchestrator is blocked, nothing settles (F-ORCH-1 status, F-ORCH-3
status).

#### H-ORCH-4 · A turn cut by the task limit lets Alondra settle what that turn was reminded of
Added at `68770a298` (#516).
What is claimed: a turn is handed a reminder (for example `question-unanswered-<q>-1`) together with work that needs
more than 10 new Tasks. At the eleventh create the store refuses with `turn_task_limit`, whose text tells the
orchestrator to end the turn (`store-sqlite/src/batch_effects.rs:89-104`). If it obeys before it gets to the reminded
item, the batch ends `Handled`: its messages become `handed`, so the reminder counts as read
(`store-sqlite/src/lib.rs:1554-1557`; `orchestrator_inbox.rs:205-215`). `handled` read `waiting` before adding the
"go on" notice (`domain_orchestrator.rs:705-707`), so with nothing else pending it settles at once (`:711`): the
question is withdrawn and its node told to decide alone, the suggestion discarded, the stopped node's case settled by
Alondra, the held plans released unchanged, although the orchestrator was stopped mid-turn by Alondra's own limit and
its next turn, started by the notice, would have handled them. Severity if confirmed: S2 (decisions taken by Alondra
that the orchestrator was about to take; a discarded suggestion is lost). Missing to confirm: whether the model, told
to end the turn, ends it before settling the other items of the batch (model behaviour, not code); the code path above
has no guard against it. Proposed test: engine-orchestrator with the orchestrator double: a batch with a reminder and a
scripted turn that creates 11 tasks and then ends; assert the reminded question is not withdrawn by that `handled`
(for example, a turn that hit the limit does not count as having read its reminders). Ablation: drop that rule.

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

```toml state-machine
id = "M-PROV-1"
code = ["crates/agent-runtime/src/owned_host.rs::launch_through", "crates/agent-runtime/src/owned_host.rs::claim", "crates/agent-runtime/src/owned_host.rs::end_holders", "agent-host/bootstrap.ts::loadBootstrap"]
truth = ["file:host.lock"]
```

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

```toml state-machine
id = "M-PROV-2"
code = ["agent-host/persistent-core.ts"]
truth = ["memory"]
```

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

```toml state-machine
id = "M-PROV-3"
code = ["agent-host/persistent-core.ts", "crates/provider-claude/src/wire.rs::check_owner"]
truth = ["memory", "sqlite:provider_active_bindings", "sqlite:provider_binding_controls", "sqlite:provider_bindings"]
```

- **Kind**: explicit in the host, mirrored in Rust. **Defined in**: host `persistent-core.ts:114-169` (`current`, `connection`, `receive`, `attach`); Rust `crates/provider-claude/src/wire.rs:96-185` (`Connection::attach`), `:426-438` (`check_owner`); the generation is reserved in the store (`reserve_owner`, see M-ENGINE-4).
- **Source of truth**: `journal.ownerGeneration` (fsynced `attached` record, `journal.ts:130-132`) and `this.owner` (live lane). In Rust: `Connection.failed` and `pending_attach.owner_generation`.
- **States per lane**: Connected (hello and challenge sent, fresh `nonce`) to Owner (attach with generation greater than the journal's and a valid HMAC proof) to Revoked (socket closed, or another lane with a higher generation, `:165-167`). In Rust: Healthy to Failed (`failed = true` after any error in `submit`, `reconcile`, `next` or `acknowledge`; sticky, `wire.rs:192-196`).
- **Guards**: `current(lane)` before each effect and after each `await` (`:114-117`); the ACK requires the generation, `cursor <= sentCursor` and `cursor >= acknowledged` (`:137`). In Rust every operation goes through `check_owner`: a failed connection prepares and sends nothing (`wire.rs:317-329`).
- **Rejected**: attach with an equal or lower generation, spent nonce or false proof destroys the socket with diagnostic `owner_message_rejected`. Any frame other than `attach` before becoming owner does the same.
- **Terminals**: Revoked. A reconnection needs a new generation (`PERSISTENCE.md`, paragraph 3).
- **Persistence and restart**: on daemon restart the host keeps running; the new daemon reserves a new generation and attaches.

### M-PROV-4 · Order and its receipt in the host journal

```toml state-machine
id = "M-PROV-4"
code = ["agent-host/journal.ts", "agent-host/persistent-core.ts", "crates/agent-protocol/src/correlation.rs::CommandRegistry", "crates/agent-protocol/src/schema.rs::ReceiptPhase"]
truth = ["memory", "sqlite:provider_commands", "sqlite:provider_control_commands"]
```

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
- **Awake clock since #541** (at `68770a298`, `agent-host/awake-clock.ts:1-43`): `awake.passed` measures a deadline in time the Mac was awake. A sleep is the wall clock running ahead of the awake clock by at least 1 s between two looks; the awake clock is now the time the busiest CPU counted (`awakeMs`: for each CPU `user + nice + sys + idle + irq`, the largest one), which stops while the Mac sleeps. Before, it was `process.hrtime`, which on macOS goes on through a sleep: an order written before a sleep was refused at wake by the rejection above, and the owner failed with "Disconnected". Assumed, not checked here: that every CPU's counters advance with wall time while the Mac is awake (if one lagged, the clock would see a false sleep, which only makes a deadline more lenient).
- **Idempotency**: same `command_id` and same digest returns the retained receipt; a different digest is a conflict and the socket is destroyed (`:172-174`). Rust never chains orders: `submit` waits for the final receipt before returning (`wire.rs:226-256`), so the assumption in `settle()` ("the owner sends nothing after an order before saving its result") holds while there is a single owner.
- **Persistence and restart**: the host stays alive, so `commands`/`settled` remain. When the daemon restarts, it reconciles `claimed` rows by resending exact bytes (`sdk_owner.rs:971-982`). For an ended host, its outbox is read as `EndedJournal` (`provider-codex/src/ended.rs`, same in Claude) and yields `Offered/Refused/AdmittedOnly/Absent`.

### M-PROV-5 · Event outbox, cursor and ACK

```toml state-machine
id = "M-PROV-5"
code = ["agent-host/journal.ts", "crates/provider-claude/src/wire.rs::LaneCursor", "crates/store/src/lib.rs::accept_observation"]
truth = ["memory"]
```

- **Kind**: explicit, composite with M-STORE-5. **Defined in**: host `journal.ts:44-63`, `:126-141`, `:158-177`; `persistent-core.ts:363-373` (`observe`), `:382-411` (`flush`); Rust `wire.rs:342-415` (`next_inner`, `acknowledge`), `:450-466` (`LaneCursor`); store `crates/store/src/lib.rs:1092-1134` (`accept_observation`).
- **Source of truth**: the host outbox (events with `cursor` = previous + 1, starting at `journal_base` in a continued host) and, in SQLite, `provider_observation_heads` plus the normalization checkpoint.
- **States per event**: Persisted, then Sent to the lane (`lane.sentCursor`), then Delivered to Rust (`LaneCursor.delivered`), then Committed in SQL (observation, checkpoint and facts in one transaction), then ACK sent, then ACK persisted in the host (the event leaves memory; the file on compaction).
- **Guards**: the first event of a new lane must have `cursor <= committed+1` and the following ones must be contiguous (`wire.rs:455-465`). The store accepts only head+1 and treats an already stored cursor with the same digest as `Duplicate` (no facts) and with another digest as `ObservationConflict`. The ACK can only cover what was delivered (`wire.rs:393-395`).
- **Redelivery after reconnection**: the host resends from `journal.acknowledged` (`:121`); what Rust already committed arrives as a duplicate and repeats no facts. There are no gaps and no undetected duplicates.
- **Invariants**: capacity is 64 MiB of un-ACKed evidence; beyond that the host marks `uncertain` and stops admitting effects (`journal.ts:73`, `persistent-core.ts:374-381`).
- **Ordering between receipts and events**: on each turn `flush` sends receipts first and then events (`:391-406`). Rust queues events that arrive while waiting for a receipt (`wire.rs:252`) and ignores stray receipts in `next` (`:351-357`). Read end to end: no consumer depends on receipt/event order.

### M-PROV-6 · Permission decision

```toml state-machine
id = "M-PROV-6"
code = ["agent-host/session.ts", "crates/store/src/controls.rs::put_decision_request", "crates/store/src/controls.rs::claim_control", "crates/store/src/controls.rs::record_control_receipt", "crates/engine-ports/src/journal/records.rs::RuleState"]
truth = ["sqlite:lifecycle_decision_requests", "sqlite:lifecycle_controls", "sqlite:allowed_rules", "sqlite:task_permission_grants"]
```

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

```toml state-machine
id = "M-PROV-7"
code = ["agent-host/session.ts", "agent-host/codex-session.ts"]
truth = ["memory", "sqlite:provider_conversation_metadata", "sqlite:provider_conversations"]
```

- **Kind**: explicit in code, in memory.
- **Claude** (`agent-host/session.ts:189-386`): `turnOpen`, set `inputs` (sends waiting for echo, max 64), `closing`, `seenResult`, `child`/`childEnded`. States: Opening (no `init`), Idle, Turn open (offer or root `assistant`), Idle (root `result`), Closing (`close()`), Finished (`finished` waits for the real exit of the native process, `:362-370`). `forceStop` = `close` + TERM + KILL with 2 s between steps (`:301-309`). Orchestrator: closes input after the first `result` with no pending sends (`:357`).
- **Codex** (`agent-host/codex-session.ts:66-560`): `opening` (`initialize -> account -> thread_resume | thread_start`), `thread`, `turn`, `queue` (max 64), `starting`, `pumping`, `closed`/`closing`, `asked`/`answered` (plans), child states. Opening deadline is 90 s (`:14`, `:164`). A queued input starts only with no open turn (`pump`, `:424-458`).
- **Source of truth**: the native process; the host only carries a technical mirror.
- **Persistence and restart**: nothing survives a host restart (the session resumes through `resume`/`thread_resume`).

### M-PROV-8 · Life of each input

```toml state-machine
id = "M-PROV-8"
code = ["agent-host/persistent-core.ts", "crates/provider-claude/src/wire.rs::submit_inner", "crates/provider-contract/src/lib.rs::Handoff"]
truth = ["derived", "sqlite:lifecycle_inputs", "sqlite:provider_input_metadata"]
```

- **Kind**: composite (host, dispatch, normalizer, domain). **Defined in**: host `persistent-core.ts:200-218` (`start`/`offer`), `session.ts:281-289`, `codex-session.ts:416-458`; Rust `wire.rs:199-257`; dispatch M-STORE-4; normalizers (`taken`/`finished` in Claude, `input_starts`/`accepted` in Codex).
- **States (observable)**:

| State | Evidence |
|---|---|
| Prepared | row `prepared`, bytes not yet recorded |
| Claimed | row `claimed` plus exact bytes (`record_command`) |
| Rejected before offer | receipt `rejected` or capability not announced: `Handoff::RefusedBeforeOffer` |
| Offered | receipt `offered`: Claude put it in the SDK `Input` queue; Codex in its `queue` (for a worker with an open thread, **only queued**, `codex-session.ts:416-422`) |
| Uncertain | `submit` failed after it could have written: `Handoff::Uncertain` (`sdk_owner.rs:1884`; `:1959` at `3fc0370ea`). Since #505 an external stop of the owner no longer produces it: `Owner::end` waits for the handoff's receipt before the host is asked to end (`:2812`, M-ENGINE-5) |
| Starting (Codex) | event `alondra/inputStarting` (`input_starts`) |
| Held | `InputHeld` (Codex `inputAccepted`) |
| Taken | `TurnStarted`/`InputConsumed` |
| Dropped | `InputDropped` (Codex `inputRefused nativeSubmitted:false`, `inputRejected`, `turn/completed` without the message; Claude `result` that names it without taking it) |

- **Double delivery**: the Claude host rejects a repeated `input_id` while it is pending (`session.ts:285`); a resend from the domain uses a new `input_id` (`HandoffResent`). Across different hosts (continuation) the pending set starts empty: what prevents double delivery is the domain's decision on a doubtful handoff (card `resend_doubtful`), not the host.
- **Zero delivery**: "Offered" does not prove the native process received it (in-memory queue of the host). If the host dies first, the ended journal says `Offered` and the domain decides on recording `HostEnded` (`resume`). This gap is exactly point 10 of the provider adapters work (phase 3) (`accepted | started | discarded | refused` confirmed by the provider); it is not repeated as a finding.

### M-PROV-9 · Claude normalizer checkpoint

```toml state-machine
id = "M-PROV-9"
code = ["crates/provider-claude/src/normalizer.rs::Checkpoint", "crates/provider-claude/src/normalizer.rs::normalize"]
truth = ["sqlite:provider_normalization_checkpoints"]
```

- **Kind**: explicit, per run. **Defined in**: `crates/provider-claude/src/normalizer.rs:44-104` (`Checkpoint`), `:202-293` (`normalize`).
- **Source of truth**: `provider_normalization_checkpoints` (one row per run, cursor = the observation's). It is rebuilt observation by observation; there is no cache.
- **State**: `turn` (`LogicalTurn`), `work`, `woken`, `taken`, `finished`, `asked`, `isolated`, `pr_commands`, `pr_close_commands`, `mcp_calls`, `listed`, `running`, `limit_resets_at`, `seen_tasks`, `initialized`.
- **Special transitions**: first cursor of a continued host: `forget_ended_host` clears `turn`, `work`, `woken`, `taken`, `finished`, `listed`, `running` (`:94-103`, `:242-244`), but **not** `asked`, `isolated`, `pr_commands`, `pr_close_commands`, `mcp_calls`, `limit_resets_at` (F-PROV-3). Last events of an ended host (`host_ended`): native exit and iterator end produce no facts (`:249-253`).
- **Rejected**: an `Unsupported`/`InvalidObservation` error from the fold restores the previous checkpoint and leaves the observation **without facts and without a record** (`:283-289`); issue #328 is open. An error of another kind propagates and the owner discards the observation with a reason (`sdk_owner.rs:2454-2459`).
- **Invariants**: accepting facts from another run is impossible by construction. The event must match task, node, conversation, instance, generation, host and fence (`:224-235`), the store requires the run to be the current one (`lib.rs:1100`), and the previous checkpoint must be at `cursor-1`.
- **Since #537 and #540** (at `68770a298`): `pr_commands` is gone. An older checkpoint's set is read into `_pr_create_commands` and never written again (`normalizer.rs:58-61`), so a `gh pr create` result emits nothing; only a successful `gh pr close` still yields `PullRequestsChanged` (`note_pr_commands`, `pull_request_facts`, `:1014-1038`). `LogicalTurn` keeps `error_detail` and `error_said` next to `error` (`:122-151`); a root assistant message without `error` after one with it clears all three and emits `ModelAnswered` if not yet emitted (`:829-855`), so the turn ends `Completed` instead of `ApiFailed` (M-ENGINE-28). Turns that end at a new `init` or at `idle` build their failure the same way (`api_failure`, `:891-894`).

### M-PROV-10 · Codex normalizer checkpoint

```toml state-machine
id = "M-PROV-10"
code = ["crates/provider-codex/src/normalizer.rs"]
truth = ["sqlite:provider_normalization_checkpoints"]
```

- **Kind**: explicit, per run. **Defined in**: `crates/provider-codex/src/normalizer.rs:18-63`, `:120-188`.
- **State**: `thread`, `process`, `turn`, `idled`, `work`, `asked`, `accepted`, `accepted_plans`, `input_starts`, `continuation(s)`, `child_*`, `model*`, `blocked_policy`.
- **Special transitions**: first cursor of a continued host clears turn, work, starts and children (`:136`); `alondra/threadReady` with another `processGeneration` also clears `asked`, `accepted` and `accepted_plans` (`:166-170`).
- **Rejected**: fold errors propagate and the owner discards the observation with a record (`discarded_observations`).
- **Invariants**: facts from another run are refused as in M-PROV-9; it also ignores events of a thread that is not the root (`fold`, `:192`).

### M-PROV-11 · Codex host process and native life

```toml state-machine
id = "M-PROV-11"
code = ["agent-host/codex-main.ts", "agent-host/persistent-core.ts"]
truth = ["memory"]
```

- **Kind**: explicit. **Defined in**: `agent-host/README.md` ("Codex process supervision"), `codex-main.ts:10-14` (`endsWithSession`, `endHostGroup`), `persistent-core.ts:313-344`; Rust `CodexWorker::native_liveness()` (`NotStarted | Alive(Member) | Lost`).
- **States**: No app-server (opens on first input), Alive, Lost. The host ends when its native session ends, waits for the ACK of the last cursor (5 s without progress, 6 s watchdog) and kills its group with the lock still held.
- **Overlap**: the Codex recovery work (phase 2), points 9-11 ("not started yet" versus "lost it", typed exit, adopted host pid).

### M-PROV-12 · State of each account

```toml state-machine
id = "M-PROV-12"
code = ["crates/core/src/accounts.rs::AccountState", "crates/core/src/accounts.rs::AccountsConfig", "crates/daemon-engine/src/accounts.rs::check_current", "crates/domain-rules/src/account_login.rs::AccountLoginPhase"]
truth = ["file:options.toml", "sqlite:account_configuration", "sqlite:account_distribution", "sqlite:account_logins", "sqlite:account_model_catalogs", "sqlite:accounts", "sqlite:model_profiles", "sqlite:task_accounts", "sqlite:settings_projection_receipts"]
states = "crates/core/src/accounts.rs::AccountState"
variants = ["Unchecked", "SignedIn", "SignedOut", "Unavailable", "Disabled", "PolicyRefused"]
```

- **Kind**: derived, snapshot. **Defined in**: `crates/core/src/accounts.rs:541-561` (`AccountState`: `Unchecked | SignedIn | SignedOut | Unavailable | Disabled | PolicyRefused`), `:453-536` (`AccountsConfig::changed`); produced in `crates/daemon-engine/src/accounts.rs:134-176` (Claude), `:522-612` (`check_current`, Codex) and `:416-418`.
- **Source of truth**: `options.toml` (`enabled`, order, policy) plus the native probe. The domain stores the snapshot (`RecordAccounts`) and each probe replaces it whole.
- **Transitions**: `PolicyRefused` is entered only by `RecordAccountPolicyRefused` (a provider refusing the account's native policy before any work) from `SignedIn`/`Unchecked`; `record_accounts` (`claude_access.rs`) keeps it while a probe reports `SignedIn`/`Unchecked` with the same identity and billing, and any other fact (another identity or billing, signed out, unavailable, turned off then on again) closes it; no timer does. Otherwise any state to any other on each probe; `Disabled` if `enabled=false`; `Unavailable` if the probe fails (`:166`, `:559`); `Unchecked` at startup or on re-enable (`:895-897`).
- **Concurrency (fence)**: the probe commit compares `accounts`, configuration versions and `options.toml` with the snapshot taken before (`commit_probe`, `:333-345`); if they changed, `stale_probe` and it is repeated. It does not include usage (`account_usage`), on purpose: readings are ordered by measurement time in `record` (F-PROV-1, fixed).
- **Missing**: no native error of a node (401, "login expired") changes the state; that is the scope of the expired provider login work.

### M-PROV-13 · Usage and limit of each account

```toml state-machine
id = "M-PROV-13"
code = ["crates/core/src/usage.rs::AccountUsage", "crates/core/src/usage.rs::record", "crates/daemon-engine/src/usage_probe.rs::measure_account", "crates/core/src/usage.rs::UsageState"]
truth = ["derived", "sqlite:account_usage"]
```

- **Kind**: implicit, derived from readings. **Defined in**: `crates/core/src/usage.rs:198-300` (`AccountUsage`, `ran_out`, `exhausted_until`), `:329-358` (`record`); `crates/domain-rules/src/rules.rs:3442-3453` (`RecordAccountUsage`).
- **States per window**: Unmeasured, then With margin (`used < FULL`, not `exhausted`), then Exhausted until `resets_at` (`exhausted` or `used == FULL`, with a future reset), then Exhausted with no known time, then With margin again (`resets_at` passes, another reading arrives, or in Codex a working turn, `native_turn_worked`).
- **Writers**: `usage_probe::measure_account` (full reading, always `exhausted:false`, `usage_probe.rs:100-121`), `usage_probe::record_limit` (orchestrator), turn observations (`store-sqlite/src/owner.rs:301-323`, `:486-497`; a turn that failed for usage holds a Codex account, and an Antigravity one only when its access reads usage with `agy -p /usage`, as a session window until its reset or that account's next reading; a key billed per use is never held, its limits being per minute), `accounts::check_current` (Codex). Codex's account check reads `account/rateLimits/read` through `ProbeReport::usage_reading` (`crates/core/src/codex_account.rs`): with `ordinaryUsageAllowed: true` it records the percentages; with `false` it records them too, plus the exhausted native-limit mark, so the account shows its current reading and stays held until a later check says `true`; with `null` (the provider does not know) it records nothing and the last reading stands. No reset time ends that hold: only Codex's answer does.
- **Invariants**: enforced in `record`: an older reading does not overwrite a newer one. A window measured after the reading's `at` stays, and a complete reading removes only windows measured before it (F-PROV-1, fixed). Readings an ended host's journal commits on adoption still carry the commit time (see F-PROV-1).

### M-PROV-14 · Orchestrator batch over the host

```toml state-machine
id = "M-PROV-14"
code = ["agent-host/persistent-core.ts", "agent-host/session.ts", "agent-host/codex-session.ts"]
truth = ["memory"]
```

- **Kind**: explicit, bounded. **Defined in**: `persistent-core.ts:189-191`, `session.ts:212-226`, `:357`; Codex `codex-session.ts` (`if (this.config.orchestrator) this.close()` when the turn ends).
- **States**: one turn per host; any later input is rejected with `single_turn`; after the `result` with no pending sends the input is closed and the host ends. The batch machine itself belongs to ORCH (M-ORCH-1/3).

### Findings

#### F-PROV-1 · A usage reading measured before a limit erases it when it arrives later: the account reads as having margin again
**Status**: FIXED in #613, as the user decided on 2026-11-22 (the latest measurement wins, by measurement time).
`usage::record` leaves a window measured after the reading's `at` as it is, and a complete reading no longer removes
a window measured after it. Test: `usage::margin_tests::a_probe_measured_before_a_limit_and_recorded_after_it_leaves_the_limit_standing`,
failing on `e1d5ec2aa` and passing with the fix. The variant of an ended host's journal (readings committed on
adoption with `at = now`) is **not** fixed: the observation carries no event time (`OrderedObservation` has none),
so ordering it needs the host to stamp its events, which is the provider adapters' work. The user decided on
2026-11-24 to refer it there; it is listed in "Referred to the tasks that own them".
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
**Status**: still holds at `68770a298`. #541 changed only how the host measures awake time (M-PROV-4): it removes the
sleep trigger of the same socket-destroying rejection (an order written before a sleep, refused at wake), not this
one. A reconciliation more than `io_timeout` of awake time after the order was prepared is still refused before
`persist` with no receipt, and `take_host_into` still propagates it (`engine-sdk/src/sdk_owner.rs:1038`, `:1043` at
`68770a298`).
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
**Status**: still holds at `68770a298` for `asked` (`forget_ended_host` `normalizer.rs:96-104`, cap `MAX_ASKED` `:20`, `:362`), `isolated`, `mcp_calls` and `pr_close_commands`. Evidence step 4 changed with #537: the `pr_commands` set and `PullRequestOpened` no longer exist (an older checkpoint's set is read and dropped, `:58-61`), so the cap now only drops later `gh pr close` commands (`MAX_PR_COMMANDS`, `:1012`, `:1019`), whose loss only skips a fresh read of the task's registered PRs.
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

```toml state-machine
id = "M-SERVER-D1"
code = ["crates/daemon-support/src/auth.rs::Device", "crates/daemon-support/src/auth.rs::Auth", "crates/daemon-support/src/auth.rs::approve_service", "crates/daemon-support/src/auth.rs::revoke"]
truth = ["file:devices.json"]
```

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

```toml state-machine
id = "M-SERVER-D2"
code = ["crates/daemon-support/src/revoked_devices.rs::RevokedDevices", "crates/daemon-support/src/revoked_devices.rs::Tombstone"]
truth = ["file:revoked-devices.json"]
```

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

```toml state-machine
id = "M-SERVER-D3"
code = ["crates/daemon-server/src/direct/pairing.rs::PairingBook", "crates/daemon-server/src/direct/pairing.rs::Request", "crates/daemon-server/src/direct/pairing.rs::Status", "crates/daemon-server/src/lib.rs::direct_approval", "crates/daemon-server/src/direct/pairing.rs::State"]
truth = ["memory", "file:direct-denied.json"]
states = "crates/daemon-server/src/direct/pairing.rs::State"
variants = ["Committed", "AwaitingApproval", "Approved", "Denied"]
terminals = ["Approved", "Denied"]
```

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

```toml state-machine
id = "M-SERVER-D4"
code = ["crates/daemon-support/src/auth.rs::Auth", "crates/client-transport/src/local.rs::Pairing", "crates/daemon-server/src/domain_bootstrap.rs::revoke_device"]
truth = ["memory", "file:tui-device"]
```

- **Kind**: implicit, two processes. **Defined in**: daemon `auth.rs:270-299` (codes in `Auth.pairing`, TTL 600 s),
  routes `domain_bootstrap.rs:621-673`; client `crates/client-transport/src/local.rs:85-196`.
- **Source of truth**: codes in daemon memory; on the client `tui-device` plus the key file, serialized across
  processes with `tui-pairing.lock` (flock; the file stays but the lock dies with the process).
- **States (client)**: `Unpaired` → (lock) code → `/pair` → deletes `tui-device` → writes the key → writes
  `tui-device` → `Paired`. A cut in the middle leaves «no device» and the next start pairs again
  (`local.rs:157-180`); the daemon keeps an orphan device «Terminal (…)» that expires by disuse.
- **Recovery**: «wrong signature» and, since G9, «device not paired» trigger one new pairing per process
  (`Rejection`, `local.rs`); the TUI says so once (F-CLIENTS-R4).

### M-SERVER-D5 · Relay enrollment

```toml state-machine
id = "M-SERVER-D5"
code = ["crates/daemon-server/src/service.rs::reserve_enrollment", "crates/daemon-server/src/service.rs::activate_enrollment", "crates/daemon-server/src/service.rs::refuse_enrollment", "crates/daemon-server/src/service.rs::revoke_enrollment", "crates/daemon-server/src/service/relay.rs::enroll"]
truth = ["sqlite:service_enrollments"]
states = "crates/daemon-server/src/service.rs::EnrollmentDecision"
variants = ["New", "Pending", "Active", "Revoked", "Refused"]
terminals = ["Revoked", "Refused"]
```

- **Kind**: explicit, SQLite. **Defined in**: `service.rs:2425-2430` (`EnrollmentDecision`), `:2638-2715`
  (`reserve/activate/revoke_enrollment`), `:2906-2915` (`enrollment_active`); loop in `service/relay.rs:228-268`
  (`enroll`), `:386-420` (`sync_revocations`), `:443-470` (`keep_registered`).
- **Source of truth**: table `service_enrollments(device_id, signing_key, state)` in `service-receipts.sqlite`;
  relay-side copies are covered by the relay part.
- **States**: `none` → `pending` → `active` | `revoked` | `refused`. `revoked` and `refused` are terminal for that id
  (a new id is needed).

| from | event [guard] | to | effect |
| --- | --- | --- | --- |
| none | reserve [no row] | pending | before any network write |
| pending | POST OK and still approved | active | |
| pending | POST OK but revoked meanwhile | revoked | DELETE on the relay |
| pending | POST 409 or 400 | refused | `GET /devices` shows `relay_refused` |
| pending | no answer, transport error, 401/403/429/5xx | pending | the round fails; the next one sends the same POST |
| refused | `sync_revocations` [no longer approved] | none | row deleted, nothing sent (the relay never held it) |
| any row not (`active`, `pending` or `refused`, and approved) | `sync_revocations` | revoked | DELETE on the relay |

  Since G11 a `pending` row is sent again (F-SERVER-D1): the relay answers a repeat of the same device, key, owner
  and Mac as the enrolment it has and refuses an ID it revoked, so the retry never revives a revoked device.
- **Effects of revoked**: deletes push bindings, Live Activity keys and preferences (`service.rs:2688-2703`).
- **Concurrency**: approval lease plus IMMEDIATE transaction.

### M-SERVER-D6 · Composite revocation

```toml state-machine
id = "M-SERVER-D6"
code = ["crates/daemon-server/src/domain_bootstrap.rs::revoke_device", "crates/daemon-server/src/device_expiry.rs::sweep", "crates/daemon-server/src/service/relay.rs::sync_revocations"]
truth = ["derived"]
```

- **Kind**: implicit. **Defined in**: `domain_bootstrap.rs:102-109` (`revoke_device`), `:792-812` (route DELETE
  `/devices/{id}`, local only; since G9 it refuses this Mac's own terminal with `own_terminal`, F-SERVER-D4);
  `device_expiry.rs:54-80` (sweep by disuse / the relay forgot the registration);
  `service/relay.rs:361-384` (account removals).
- **Steps**: the daemon records the revocation locally (M-SERVER-D1, M-SERVER-D2), reloads its devices, and the
  relay loop propagates it to the relay (`sync_revocations`). The keys stay in `service.toml` on purpose.

### M-SERVER-D7 · iroh fingerprints

```toml state-machine
id = "M-SERVER-D7"
code = ["crates/daemon-server/src/iroh_terminals.rs::IrohTerminals", "crates/daemon-server/src/service/relay.rs::keep_device_iroh_nodes_bound"]
truth = ["file:iroh-terminals.json"]
```

- **Kind**: implicit. **Defined in**: `iroh_terminals.rs:20-89`; `service.rs:957-1027`; `service/relay.rs:300-350`.
- **Source of truth**: `iroh-terminals.json` (device_id → node + hello signature); bindings already made on the
  relay are in memory (`relay.devices_bound`). Pruned on read (`hellos(keep)`) to the still-approved devices. Every
  iroh stream goes through `handle_direct`, which checks the approval again.

### M-SERVER-D8 · Push recipients

```toml state-machine
id = "M-SERVER-D8"
code = ["crates/daemon-server/src/service.rs::activity_phones", "crates/daemon-support/src/push.rs::Push", "crates/daemon-server/src/activity_push.rs::Phone"]
truth = ["derived", "sqlite:service_activity_keys", "sqlite:service_push_bindings", "sqlite:service_push_preference_revisions", "sqlite:service_push_preferences"]
```

- **Kind**: implicit.
- **Encrypted (service)**: `service.rs:1797-1850`: for each device in `service.toml`, approved in memory plus
  enrollment `active` plus binding. Live Activity: `activity_push.rs:149-160` replaces the phone list on every
  round.
- **Legacy (direct APNs)**: `daemon-support/src/push.rs:48-118`, used when there is no `service.toml`
  (`domain_delivery.rs:244-248`, `push_alerts.rs:40-42`).

### M-SERVER-D9 · Self-update

```toml state-machine
id = "M-SERVER-D9"
code = ["crates/daemon-support/src/updates.rs::plan", "crates/daemon-support/src/updates.rs::Updater", "crates/daemon-support/src/updates.rs::record_attempt"]
truth = ["file:auto-update.json"]
```

- **Kind**: implicit. **Defined in**: `daemon-support/src/updates.rs:17-118`; `alondra_core::update::{Attempt,
  Status}`.
- **Source of truth**: `auto-update.json` (last `Attempt{version, at, ok, detail, attempts}`, written to a temporary
  file and renamed since G11); `KNOWN` in memory (last version seen).
- **States**: `Up to date` | `Available` → `Updating (brew)` → `Attempted(version, ok, attempts)`. Since G11
  (F-SERVER-D7) `plan()` upgrades a version again after a failure once `Attempt::retry_at` passes (30 min, 2 h,
  8 h after the 1st, 2nd and 3rd failure) and never after a success or the fourth failure (`gave_up`): that
  version then waits for the next one. The check loop sleeps until the next retry when it comes before 6 h.

### M-SERVER-D10 · Relaunch after the binary is replaced

```toml state-machine
id = "M-SERVER-D10"
code = ["crates/daemon-support/src/relaunch.rs::Watcher", "crates/daemon-support/src/relaunch.rs::watch", "crates/alondrad/src/lib.rs::relaunch_now"]
truth = ["memory", "file:startup-failures.json"]
states = "crates/daemon-support/src/relaunch.rs::Check"
variants = ["Unchanged", "Relaunch", "Rejected"]
```

- **Kind**: explicit. **Defined in**: `daemon-support/src/relaunch.rs:113-232`; `alondrad/src/lib.rs:170-182`,
  `:329-347`.
- **States**: `Running(identity)` → binary changed → preflight → `Relaunch` (exits with 75 via `exit::now`, without
  draining) | `Rejected(identity, t)` (retried after 10 min). If the binary changes during preflight, the result is
  discarded (`:147-149`). `startup-failures.json` (host crate) counts failed starts.

### M-SERVER-D11 · Service HTTP challenge verifier

```toml state-machine
id = "M-SERVER-D11"
code = ["crates/e2e/src/lib.rs::HttpChallengeVerifier"]
truth = ["memory"]
```

- **Kind**: explicit. **Defined in**: `crates/e2e/src/lib.rs:488-612`; used in `auth.rs:347-369`
  (`/service/approve-device`).
- **State**: random per-process `epoch`, nonces used until `timestamp+skew`, clock high-water mark (a clock that
  goes backwards stops accepting requests until it catches up). The nonce is recorded only with a valid signature.

### M-SERVER-D12 · Pending service pairing (CLI, context only)

```toml state-machine
id = "M-SERVER-D12"
code = ["crates/cli/src/service_pair.rs::Pending", "crates/cli/src/service_pair.rs::complete_pending", "crates/cli/src/service_pair.rs::abandon_pending"]
truth = ["file:service-pairing-pending.json"]
```

- `cli/src/service_pair.rs:390-538`: `service-pairing-pending.json` (≤10 min, `relay/src/pairing.rs:12`); continue
  or abandon (abandoning revokes through DELETE `/devices/{id}` on the daemon). A 410 from the daemon (tombstone)
  makes it discard the pending pairing.

### Findings (devices part)

#### F-SERVER-D1 · A transient failure on the first enrollment permanently revokes a device's relay access while it still shows as paired
**Class**: CONFIRMED · **Severity**: S2 · **Probability**: medium-low (only on the first POST of each device, right after pairing) · **Overlap**: none known.
**Status**: FIXED in group G11 (issue #528), validated in #628. `enroll` sends a `pending` row again (the relay answers a repeat of the same device, key, owner and Mac as the enrolment it has), `sync_revocations` leaves `pending` and `refused` rows of approved devices alone, and a 409 or 400 leaves the row `refused`, shown in `GET /devices` as `relay_refused` and in the CLI device list as «the relay refused it: pair it again from the device». No compensating DELETE: it would revoke the ID at the relay for good. Tests: `daemon-server` `service::tests::a_first_enrollment_without_a_settling_answer_is_sent_again_until_the_relay_takes_it` (503 then 201: two POSTs, no DELETE, `active`) and `an_enrollment_the_relay_refuses_says_to_pair_again_and_is_not_sent_again`; they replace `ambiguous_enrollment_is_deleted_and_never_retried`, which pinned the old behaviour. Ablation: without sending a `pending` row again the first test fails (`enrollment_active` is false after the second round), and without the 409/400 branch the second fails (`409 Conflict` comes back as an error); restored, both pass.
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
**Status**: FIXED in group G9 (issue #526), with the default decision (not allowed). The device list (`GET /devices`, which the Mac app's Settings and the CLI's device list read) leaves out the device in `tui-device`, and `DELETE /devices/{id}` refuses it with `own_terminal` (409) before touching anything (`domain_bootstrap.rs`, `device_expiry::own_terminal`). A terminal already removed pairs again by itself (F-CLIENTS-R4). Test: `daemon-server` `domain_bootstrap::tests::this_macs_own_terminal_is_neither_listed_nor_unpaired`. Ablation: without the guard in `revoke_device` the test fails (200 and the terminal unpaired); restored, it passes.
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
**Status**: FIXED in group G11 (issue #528), validated in #628. `direct_approval` now gives the pairing book an `admit` check, read without writing: an ID with a tombstone is `Denied` (403 `pairing_denied`) and one that `devices.json` or `service.toml` holds with other keys is `Conflict` (409 `pairing_conflict`, typed `ServiceApprovalError::OtherKey`). `start` asks it before booking, so such a request never reaches the user; `approve` asks it again and a refusal ends the request `denied` without writing anything. Tests: `daemon-server` `direct::tests::a_revoked_or_conflicting_id_is_refused_before_it_reaches_the_user` and `direct::pairing::tests::a_device_revoked_while_it_waits_ends_denied_instead_of_expiring`. Ablation: without `admit` in `start` and `approve`, the network test gets 200 with a pairing id instead of 403 `pairing_denied`, and the unit tests get `Ok` instead of `Err(Denied)`; restored, they pass.
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
**Status**: FIXED in group G11 (issue #528), validated in #628, with the default decision: 3 retries per version with a growing wait (30 min, 2 h, 8 h), then wait for the next version. `Attempt` counts `attempts` (a file without it reads as 1); `plan` takes the time and `Attempt::retry_at`; `alondra doctor` says whether it will be tried again or waits for the next version; `auto-update.json` is written to a temporary file and renamed. Test: `daemon-support` `updates::tests::a_failed_upgrade_is_tried_again_three_times_with_a_growing_wait_then_waits_for_the_next_version`. Ablation: with `plan` back to «never the same version twice» the test fails (`Nothing` instead of `Upgrade` at the first retry); restored, it passes.
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

```toml state-machine
id = "M-SERVER-R1"
code = ["crates/relay/src/pairing.rs::Mailbox", "crates/relay/src/pairing.rs::Session"]
truth = ["memory"]
```

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

```toml state-machine
id = "M-SERVER-R2"
code = ["crates/relay/src/auth.rs::DeviceRegistry", "crates/relay/src/auth.rs::persist_registry_at", "crates/relay/src/auth.rs::enroll_mac"]
truth = ["file:registry.json"]
```

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

```toml state-machine
id = "M-SERVER-R3"
code = ["crates/relay/src/auth.rs::register_push", "crates/relay/src/auth.rs::unregister_push", "crates/relay/src/auth.rs::forget_dead_push"]
truth = ["derived"]
```

- **Kind**: implicit: `push[device]` + `push_sessions[device]` + `push_ended ∋ device`. `auth.rs:89-92`.
- **States**: `None` (never registered) / `Registered(token, binding, session?)` / `Ended` (the relay ended it on
  purpose, so the Mac receives `410` and can forget the binding, `lib.rs:593-599`).
- **Events**: `register_push` (supersedes registrations of the same token with the same Mac and owner,
  `auth.rs:596-651`); `unregister_push` (with the binding, 654-675); `forget_ended_session_push` (sweep of offline
  Keycloak sessions every 15 min and operator, 710-728, compares `sid`); `forget_dead_push` (APNs says token dead;
  compares the token, 758-772); revocations (delete push and insert `push_ended`).
- **Invariant**: a revoked device keeps no push (every revocation route deletes it in the same write).

### M-SERVER-R4 · Account removal sequence from the account source

```toml state-machine
id = "M-SERVER-R4"
code = ["crates/relay/src/auth.rs::RemovalsApplied", "crates/relay/src/auth.rs::apply_removals"]
truth = ["derived"]
states = "crates/relay/src/auth.rs::RemovalsApplied"
variants = ["Through", "Gap", "Behind"]
```

- **Kind**: explicit (`RemovalsApplied::{Through,Gap,Behind}`), `auth.rs:162-170, 893-960`; `removals_pending:
  AtomicBool` (until the first reconciliation, every route that looks at the account answers unavailable,
  `auth.rs:968-974, 1436-1445`).
- **Source of truth**: `removals_seq` in the registry JSON; the external source sends the full list.
- **Transitions**: a batch with `seq ≤ held` is ignored (idempotent); a gap → 409 and a notice to the follower to
  read the full list (`operator.rs:155-163`); a full list adopts its number even if lower. A removal for another
  account is skipped and logged. Nothing is ever restored. Correct as read.

### M-SERVER-R5 · iroh endpoint binding

```toml state-machine
id = "M-SERVER-R5"
code = ["crates/relay/src/auth.rs::bind_iroh_node", "crates/relay/src/auth.rs::store_iroh_node", "crates/relay/src/auth.rs::iroh_node_holder"]
truth = ["derived"]
```

- **Kind**: implicit (`iroh_nodes[id]` + `proven_iroh_nodes ∋ id`), `auth.rs:97-99, 1010-1134`.
- **States**: `Unbound` / `BoundOnMacWord` / `BoundProven`. A proof signed by the endpoint key replaces a binding
  made on the Mac's word; nothing removes a proven binding. `iroh_node_holder` returns a holder only if the Mac is
  still enrolled; then `require_live_route` applies (tombstones and account).

### M-SERVER-R6 · Live Activity per phone

```toml state-machine
id = "M-SERVER-R6"
code = ["crates/relay/src/activity.rs::Phone", "crates/relay/src/activity.rs::State"]
truth = ["file:live-activity.json"]
```

- **Kind**: composite. **Defined in**: `Phone` `activity.rs:146-172`, `State` 174-185, plan `Phone::plan` 266-279,
  `take` 549-576, `settle` 580-620.
- **Source of truth**: `live-activity.json` plus memory behind a `Mutex`, separate from the registry. `scheduled`
  (in memory) avoids two grouped sends.
- **States of `live`**: `None` / `Some(no update_token)` (started by the relay or by iOS with the app force-quit;
  cannot be ended, and blocks a new `Start` until rotation) / `Some(with token)`. Product with `reports` (active =
  any report with `active`), `pairings`, `held`, `ended`, `unsent`, `urgent_pending`, `last_*`.
- **Events**: `register`/`unregister` (phone), `record` (Mac), `take` (records the send before APNs answers),
  `settle` (APNs response), `prune` (after revocations).
- **Persistence**: everything; a write failure rolls back the in-memory change (`activity.rs:409-420`). Since G11
  the first `retry_owed` round, at start, and every round after it prune against the registry (F-SERVER-R4), and
  `Phone::start_unanswered` marks a start recorded before APNs answered: `Store::open` drops the activity it
  stands for (F-SERVER-R6).

### M-SERVER-R7 · Web login flow (OIDC)

```toml state-machine
id = "M-SERVER-R7"
code = ["crates/relay/src/web_session/store.rs::create_flow"]
truth = ["postgres:web_login_flows"]
```

- **Kind**: `web_login_flows` rows (PostgreSQL), `web_session/store.rs:161-199`.
- **States**: `Created` (10 min) → `Consumed` (atomic `DELETE … RETURNING` with the browser cookie **and** `state`,
  190-199) / `Expired` (deleted by `create_flow` and `reconcile`).
- **Invariants**: single consumption (two concurrent callbacks: only one gets the row); the ID token `nonce` is
  validated in the exchange (`oidc.rs:186`).

### M-SERVER-R8 · Web session

```toml state-machine
id = "M-SERVER-R8"
code = ["crates/relay/src/web_session/store.rs::begin_refresh", "crates/relay/src/web_session/store.rs::retain_rotated", "crates/relay/src/web_session/store.rs::activate_rotated"]
truth = ["postgres:web_sessions"]
```

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

```toml state-machine
id = "M-SERVER-R9"
code = ["crates/relay/src/management/mod.rs::Invitation", "crates/relay/src/management/mod.rs::reserve_acceptance", "crates/relay/src/management/mod.rs::reissue_invitation"]
truth = ["postgres:management_invitations"]
```

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

```toml state-machine
id = "M-SERVER-R10"
code = ["crates/relay/src/management/mod.rs::Account", "crates/relay/src/management/mod.rs::change_account_access"]
truth = ["postgres:management_accounts", "postgres:management_identities"]
```

- **Kind**: explicit `status ∈ {provisioning, active, disabled, deleting, deleted}` (CHECK
  `0001_management.sql:4`). `change_account_access` `mod.rs:536-597`, `reconciliation_complete` 863-871.
- **Transitions**: `provisioning→active` (only `complete`); `active|provisioning→disabled|deleting`;
  `disabled→deleting`; `deleting→deleted` when all its outbox is completed. There is no reactivation path
  (`disabled→active`): terminal in practice. The last-admin invariant is protected by `ADMIN_LOCK` + `FOR UPDATE`.

### M-SERVER-R11 · Account reconciliation outbox

```toml state-machine
id = "M-SERVER-R11"
code = ["crates/relay/src/management/mod.rs::Reconciliation", "crates/relay/src/management/mod.rs::reconciliation_complete", "crates/relay/src/management/worker.rs"]
truth = ["postgres:management_outbox"]
```

- **Kind**: `management_outbox(state ∈ {pending, completed}, available_at, attempts)` rows, unique `(account_id,
  action)`. `mod.rs:207-211, 825-871`, worker `management/worker.rs`.
- **Semantics**: claim with a 60 s lease (`FOR UPDATE SKIP LOCKED` + `available_at`), idempotent external
  operations; `keycloak_delete` waits until no other action is pending; completed rows of inactive accounts are set
  back to `pending` every 5 min **forever** (desired tombstones, including `deleted`): periodic work of 3×N deleted
  accounts. A design decision documented in the code.

### M-SERVER-R12 · Device revocation tombstones (account)

```toml state-machine
id = "M-SERVER-R12"
code = ["crates/relay/src/management/devices.rs::DeviceRevocation", "crates/relay/src/management/devices.rs::require_live_device"]
truth = ["postgres:management_device_revocations"]
```

- **Kind**: `management_device_revocations(state ∈ {pending, completed})` rows, PK `(kind, device_id)`.
  `management/devices.rs:17-122`.
- **Semantics**: the tombstone is authority once confirmed (`require_live_device` consults it on every authenticated
  route); the worker applies the cleanup to the JSON under the write gate (`worker.rs:21-31`), purges pairings and
  activity, and marks `completed`; it is reviewed again every 5 min. Idempotent on reapply.

### M-SERVER-R13 · Waitlist

```toml state-machine
id = "M-SERVER-R13"
code = ["crates/relay/src/management/waitlist.rs::WaitlistEntry", "crates/relay/src/management/waitlist.rs::invite_from_waitlist"]
truth = ["postgres:management_waitlist"]
```

- Row present/absent (`management_waitlist`, unique email). `invite_from_waitlist` deletes the row and issues the
  invitation in the same admin transaction (`waitlist.rs:297-321`): a conflict leaves both as they were. Correct.

### M-SERVER-R14 · Admin request idempotency

```toml state-machine
id = "M-SERVER-R14"
code = ["crates/relay/src/management/mod.rs::AdminRequest"]
truth = ["postgres:management_admin_requests"]
```

- `management_admin_requests(actor_id, key_hash)` with the request hash (`mod.rs:213-274`). Reserved inside the
  operation's transaction: if the operation fails, the reservation is rolled back. Retry with the same key:
  `AlreadyApplied`, or `TokenAlreadyIssued` for those that issue a token (the token is not stored: it is lost).

### M-SERVER-R15 · JWKS cache

```toml state-machine
id = "M-SERVER-R15"
code = ["crates/relay/src/jwt.rs::Verifier", "crates/relay/src/jwt.rs::refresh_for_unknown_kid"]
truth = ["memory"]
```

- `jwt.rs:49-206`: fresh up to `JWKS_MAX_AGE`; an unknown `kid` forces a limited reload; reload every 5 min
  (`lib.rs:1029-1037`). No invalid states observed.

### Findings (relay part)

#### F-SERVER-R4 · If Live Activity pruning fails after a revocation, the removed Mac keeps the Live Activity alive indefinitely
**Class**: CONFIRMED (deterministic path on a write failure or a crash between two writes) · **Severity**: S2 (visible, recoverable with another revocation) · **Probability**: low (disk failure or crash in a short window) · **Overlap**: none known.
**Status**: FIXED in group G11 (issue #528), validated in #628. `retry_owed` starts every round, the first one at start, with `retry_round`, which prunes against the registry (`after_revocation`) under the device gate before sending what is owed. Test: `relay` `activity_http_tests::a_revoked_mac_whose_reports_could_not_be_forgotten_leaves_on_the_next_round` (the write of `live-activity.json` fails because a directory takes its temporary file's place; the next round drops the revoked Mac). Ablation: without the prune in `retry_round` the test fails (`["mac-1", "mac-2"]` instead of `["mac-2"]`); restored, it passes.
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
**Status**: FIXED in group G11 (issue #528), validated in #628. `apply(Start)` sets `Phone::start_unanswered`, `settle` and `register` clear it, and `Store::open` takes a start still unanswered as never sent: it drops the activity without a token and leaves the phone owing a push, so the next start waits only the 10-minute start spacing. Tests: `relay` `activity::tests::a_start_the_relay_stopped_before_apns_answered_does_not_hold_the_next_one` and `a_start_apns_answered_stays_started_across_a_restart`. Ablation: without `forget_unanswered_start` in `Store::open` the test fails (`[]` instead of `[Start]`); restored, it passes.
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
**Status** (G11, issue #528): still a hypothesis, not changed. The web session store is PostgreSQL itself
(`web_session/store.rs`, no port or double), so the proposed test needs a real database, which the test rules of
the fix groups do not allow. Confirming it needs a store port with a double, or an opt-in PostgreSQL run.
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

```toml state-machine
id = "M-CLIENTS-R1"
code = ["crates/cli-tui/src/tui/optimistic.rs::Optimistic", "crates/cli-tui/src/tui/optimistic.rs::Sending", "crates/cli-tui/src/tui/optimistic.rs::Applied"]
truth = ["memory"]
```

- **Kind**: explicit (struct with two maps). **Defined in**: `crates/cli-tui/src/tui/optimistic.rs:24-171`; used in `crates/cli-tui/src/tui.rs:1605-1654` (send), `2356-2442` (answer), `2444-2483` (timer), `2075` (reconcile).
- **Source of truth**: TUI process memory, one per Mac tab (`App.optimistic`, `tui.rs:759`). Since G9 (issue #526) the automatic restart carries the `Sending` entries of every tab to the new process in `Restore::unconfirmed` (`MacAnswers` per Mac id), and `take_over` resumes them due at once. **Copies and derivatives**: the daemon keeps a receipt per `command_id` (UUID v4, `client.rs:783-789`).
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
- **Invariants**: resending reuses the `command_id` (enforced in `due`/`same_action`, and across a restart by `Optimistic::resume`). Nothing ends the process with live `Sending` entries unsaid: the restart carries them and a manual quit asks first (`quits`, `tui.rs`); a second quit quits.
- **Applied entries** (since G9): `Applied` also keeps `asked_before`, the last view ticket issued when the daemon settled it; a view answering a ticket at or below it cannot bring the card back.
- **Persistence and restart**: the automatic restart keeps the `Sending` entries (above); `Applied` entries are not carried (the new process reads a fresh view). A manual quit confirmed over them loses them.
- **Concurrency**: only the main thread mutates; POSTs run in threads (`spawn`, `tui.rs:1799-1811`) and come back through a channel. Two POSTs of the same command can be in flight at once (equal submit during an attempt in flight); the daemon deduplicates by `command_id`.

### M-CLIENTS-R2 · Per-tab connection state of the TUI

```toml state-machine
id = "M-CLIENTS-R2"
code = ["crates/cli-tui/src/tui.rs::MacStatus", "crates/cli-tui/src/tui.rs::App"]
truth = ["memory"]
```

- **Kind**: implicit.
- **Fields**: `connected`, `offline`, `loaded` (`tui.rs:737-739`), `view_status`, `home_view`, `screen_view_asked`, and, outside `App`, the live transport route `DirectMac::route_kind()` (`direct_client.rs:288-291`, `323-336`).
- **Effective states**: Connecting (`!connected && !offline`), Connected (`connected && !offline`), Offline (`offline`). `connected && offline` is never built because both are always written together (`tui.rs:2086-2087`, `2100-2101`, `2191-2192`, `2196-2197`, `2211-2212`).
- **Writers**: `ViewHome(ticket, Ok/Err)` (from `load_screen_view` and from the feed thread), `ViewFeed(Ok/Err)`, `Msg::Offline` (no sender: `background_sync_error` has no callers, `tui.rs:915-921`). Since G9 every screen view request takes a ticket from `App.view_tickets`, shared with the feed thread, and `home_ticket` keeps the newest answered: an answer to an older request is dropped (F-CLIENTS-R3).
- **Derivatives read by the UI**: tab (`MacStatus`, `tui.rs:258-293`, `1398-1424`: uses `offline`, `connected` and `route_kind`), red header band (`draw.rs:350-363`: `offline` and `view_status`), waiting screen (`draw.rs:618-633`: `offline`, `loaded`), terminal title (`tui.rs:969`). Three sources: the two booleans and the transport route, which can be `None` with `connected=true` (see F-CLIENTS-R2).
- **Persistence and restart**: none; there is no home cache on disk (overlap #387).

### M-CLIENTS-R3 · Availability probe (`AvailabilityProbe` / `ResponseObserver`)

```toml state-machine
id = "M-CLIENTS-R3"
code = ["crates/client-port/src/client/availability.rs::AvailabilityProbe", "crates/client-port/src/client/availability.rs::ResponseObserver"]
truth = ["memory"]
```

- **Kind**: explicit. **Defined in**: `crates/client-port/src/client/availability.rs:20-139`. State `{responded, finished}` per request and one shared slot per destination.
- **Transitions**: `begin_request` sets responded=false; `received` (headers arrive before the deadline) sets responded=true; `finish` when the thread ends. `check` reuses the active probe if it has not expired and does not launch a second thread.
- **Invariants**: a late response (after the deadline) does not count (`received` checks the deadline). Any HTTP response counts as "responds", including a 401/421/503 or the response of another Mac on the same IP.

### M-CLIENTS-R4 · Automatic TUI restart on a new daemon

```toml state-machine
id = "M-CLIENTS-R4"
code = ["crates/cli-tui/src/tui/version_watch.rs::decide", "crates/cli-tui/src/tui/version_watch.rs::Decision", "crates/cli-tui/src/tui/updated.rs::Watch", "crates/cli-tui/src/tui.rs::restart"]
truth = ["memory"]
```

- **Kind**: explicit. **Defined in**: `version_watch::decide` (`crates/cli-tui/src/tui/version_watch.rs:79-104`) over `Watch` (`updated.rs:16-32`); effect in `main_loop` (`tui.rs:1027-1031`) and `restart` (`tui.rs:1043-1062`).
- **Decisions**: Nothing, CheckDisk, Warn(...), Countdown, Restart. Restart when the daemon is newer, the binary on disk is that version, no restart was already done for it, and (requested with R) or (idle >= 10 s and the 3 s countdown elapsed), provided it is not `busy` or `confirming`.
- `busy` = `actions_out > 0` or an attachment uploading (`updated.rs:78-84`). `actions_out` counts only requests in flight: +1 in `action_sent`, -1 when the response arrives (`tui.rs:2066-2068`, `2202-2203`).
- **Persistence and restart**: `Restore` in `$TMPDIR` (screen, selection, composer, and since G9 the unconfirmed answers of every Mac tab, F-CLIENTS-R1). An unconfirmed answer between two attempts does not hold the restart; it travels.

### M-CLIENTS-R5 · Pairing notices in the TUI

```toml state-machine
id = "M-CLIENTS-R5"
code = ["crates/cli-tui/src/tui.rs::App", "crates/cli-tui/src/tui.rs::PAIRING_POLL"]
truth = ["memory"]
```

- **Kind**: implicit. **Defined in**: `pairing: Vec<PendingDevice>` plus `pairing_answered: HashSet` (`tui.rs:797`, `1426-1452`, `2230-2261`). Local feed every `PAIRING_POLL`, sends only on change.
- **Transitions**: answered, removed and marked; error, reinserted and unmarked; the feed removes the marks of devices no longer pending. Correct.

### M-CLIENTS-R6 · Local terminal signature (pairing on first use)

```toml state-machine
id = "M-CLIENTS-R6"
code = ["crates/client-transport/src/local.rs::Pairing", "crates/client-transport/src/local.rs::send"]
truth = ["file:tui-device"]
```

- **Kind**: explicit/composite. **Defined in**: `Pairing{signer cache, replaced}` plus files `tui-device`/`tui-key.pem` plus `tui-pairing.lock` (`crates/client-transport/src/local.rs:85-191`, `send` at `273-284`).
- **States**: no pair, pairing (POST /pairing + /pair, device-last write), pair saved, pair cached. Repair: when the daemon answers 401 `"wrong signature"` or, since G9, `"device not paired"` (`Rejection`), once per process (`replace_rejected`); the second one leaves a notice the TUI shows once (`Transport::local_notice`).
- **Invariants**: enforced: the device file is deleted before the key is written and written last, so a cut never leaves the key of one pair with the device of another.
- **Rejected**: either reason above after the one repair of the process.

### M-CLIENTS-R7 · Direct connection to a Mac (`Connection`)

```toml state-machine
id = "M-CLIENTS-R7"
code = ["crates/client-transport/src/direct_client.rs::Connection", "crates/client-transport/src/direct_client.rs::IrohLink", "crates/client-transport/src/iroh_client.rs::Backoff", "crates/client-transport/src/iroh_client.rs::RelayWatch"]
truth = ["file:direct-macs.json"]
```

- **Kind**: composite. **Defined in**: `direct_client.rs:252-600`; backoff in `iroh_client.rs:257-273`. Current HTTP route (`current: Mutex<Option<String>>`), iroh link `IrohLink{Untried, Open, Down}` plus `Backoff` plus `RelayWatch`, `learned`. **Persistence**: `direct-macs.json` (routes, keys) and the iroh "dial" per Mac, under `direct-macs.lock`.
- **Main transitions**: Untried to Open (connects) or Down (+backoff); Open to Untried (send failure) or Down (`wrong_mac`, 60 s retention); Open stuck on relay to a new connection from a new endpoint. HTTP: a known route is used without `hello`; a new route is validated with `hello` (OtherMac is skipped, KeyChanged aborts).
- **Idempotency**: the same sealed envelope (same `request_id`) goes over iroh and, if lost midway, over HTTP; the Mac executes a `request_id` once. Each new call seals a new `request_id`.
- **Rejected**: HTTP response classification: a 200 with an openable envelope is the answer; since G9 a non-200 whose body opens as a refusal the Mac sealed (`refusal` field) is read as over iroh (`refused`: `unauthorized`, `expired`, `idempotency_conflict`, `busy` = not run, other = answered) and keeps the route; anything else counts as "sent without answer" (F-CLIENTS-R2).

### M-CLIENTS-R8 · Direct pairing with a Mac (`direct_client::Pairing`)

```toml state-machine
id = "M-CLIENTS-R8"
code = ["crates/client-transport/src/direct_client.rs::Pairing", "crates/client-transport/src/direct_client.rs::connect"]
truth = ["memory"]
```

- **Kind**: explicit by steps. **Defined in**: `direct_client.rs:645-790`, and `connect` (`824-865`).
- **Steps**: start (POST /direct/pairing), reveal, wait (poll 1 s, up to 130 s), then approved (verifies the signature against `hello`) | denied | expired. `connect` saves with `remember` only after verifying.
- **Half-done case**: any transient error in the poll (`.call()?`, `direct_client.rs:760`) aborts and discards the keys; see H-CLIENTS-R1.

### M-CLIENTS-R9 · Terminal relay profile and relay epoch

```toml state-machine
id = "M-CLIENTS-R9"
code = ["crates/client-transport/src/service_client.rs::Profiles", "crates/client-transport/src/service_client.rs::EpochCache", "crates/client-transport/src/client_pair.rs::pair"]
truth = ["file:terminal-service.json"]
```

- **Defined in**: `service_client.rs:29-201`; invitation pairing in `client_pair.rs:110-232` (candidate, approval poll, verification of the signed proof).
- **State**: `Profiles` in `terminal-service.json` under `terminal-service.lock`; process target in `ACTIVE: OnceLock` (immutable per process); `EpochCache{generation, value}`.
- **Retries**: only GET after a 401 with a proven new epoch; POST is never repeated (`service_client.rs:127-147`).
- Used only by the Mac-connect command and by the delivery command with `--mac`.

### M-CLIENTS-R10 · Account session in the CLI

```toml state-machine
id = "M-CLIENTS-R10"
code = ["crates/cli/src/account.rs::SessionState", "crates/cli/src/account.rs::AccountLock", "crates/cli/src/account.rs::SERVICE_TOKEN_CACHE"]
truth = ["derived"]
states = "crates/cli/src/account.rs::SessionState"
variants = ["Active", "PendingRevocation"]
```

- **Kind**: explicit. **Defined in**: `SessionState{Active, PendingRevocation}` in the keychain plus `AccountLock` (per-user global flock) plus `SERVICE_TOKEN_CACHE` (`crates/cli/src/account.rs:69-83`, `464-535`, `596-656`, `896-1012`).
- **Transitions**: login (revokes and deletes the previous one, then saves Active); refresh (persist the rotated refresh token before `userinfo`, `508-535`); logout: Active to PendingRevocation (persisted) to revocation to deletion; resumable.
- **Rejected**: only `invalid_grant` (400/401) is treated as an expired session (`493-506`); network failures keep the session.
- Keycloak with `revokeRefreshToken=false` (`apple/Data/Account/Sources/Auth/OIDCConfiguration.swift:37-41`): the predecessor stays valid, so two concurrent refreshes do not drop the session. No findings.

### M-CLIENTS-R11 · Device pairing from the Mac (device invite command)

```toml state-machine
id = "M-CLIENTS-R11"
code = ["crates/cli/src/service_pair.rs::Pending", "crates/cli/src/service_pair.rs::complete_pending", "crates/cli/src/service_pair.rs::abandon_pending"]
truth = ["file:service-pairing-pending.json"]
```

- **Kind**: composite and durable. **Defined in**: `service-pairing-pending.json` plus the device in `service.toml` plus local approval plus registration in the relay plus proof handover (`crates/cli/src/service_pair.rs:390-541`, `625-787`).
- **Resumable**: if a non-expired pending exists, `complete_pending` repeats the idempotent steps; if it expired, `abandon_pending` revokes in the relay (tombstone first), removes it from config, deletes local state and deletes the pending. A 410 from local approval (device removed) abandons. No findings; the pending is only resumed when the command is run again.

### M-CLIENTS-R12 · Delivery of MCP calls from nodes and the orchestrator

```toml state-machine
id = "M-CLIENTS-R12"
code = ["crates/cli-mcp/src/mcp.rs::submit", "crates/cli-mcp/src/mcp.rs::send_command", "crates/cli-mcp/src/mcp.rs::send_command_once"]
truth = ["memory"]
```

- **Kind**: explicit per call. **Defined in**: `crates/cli-mcp/src/mcp.rs:973-1104`. `submit` (waits while the daemon is older, up to 1 h, resending the same command) to `send_command` (cards: up to 3 sends 1 s apart; others: 1) to `send_command_once` (reads `data_epoch` from /capabilities and sends).
- **`command_id`**: cards with `interaction_id` use `mcp-{tool}-{interaction_id}` (`1149-1160`); others use `mcp-native-sha256(operation, tool)`, with operation = hash of native `threadId/sessionId/callId` or of `{server instance, JSON-RPC id}` (`1137-1147`, `1873-1883`).
- **Receipts** at the daemon per `(actor, command_id, data_epoch)` with a payload digest: same payload gives `replayed`; different gives `idempotency_conflict` (`crates/store-sqlite/src/command.rs:111-120`). A refusal leaves no receipt, so resending corrected with the same `interaction_id` works.
- **Quoted images** are copied by SHA-256 (`crates/client-port/src/doc_images.rs:63-95`, `122-135`), so an identical resend produces the same payload. No confirmed findings; see H-CLIENTS-R2/R3.

### M-CLIENTS-R13 · Delivery failure classification (shared)

```toml state-machine
id = "M-CLIENTS-R13"
code = ["crates/client-port/src/client.rs::Delivery", "crates/client-port/src/client.rs::CommandFailure", "crates/client-port/src/client.rs::delivery"]
truth = ["derived"]
states = "crates/client-port/src/client.rs::Delivery"
variants = ["Unsent", "Unanswered", "Running", "Unknown", "Answered"]
```

- `Delivery{Unsent, Unanswered, Running, Unknown, Answered}` and `CommandFailure{Rejected, Uncertain}` (`crates/client-port/src/client.rs:698-766`), plus `transport.delivery` (`client-transport/src/lib.rs:98-101`, `local.rs:316-328`). The TUI, the CLI and the MCP use it to decide whether to retry.

### M-CLIENTS-R14 · Sending a crash report (`sending.json`)

```toml state-machine
id = "M-CLIENTS-R14"
code = ["crates/cli-report/src/report/send.rs::Sending", "crates/cli-report/src/report/send.rs::send_to", "crates/cli-report/src/report/send.rs::keep", "crates/cli-report/src/report/send.rs::postpone", "crates/daemon-support/src/reports.rs::retry_pending", "crates/daemon-engine/src/report_held_card.rs::check"]
truth = ["file:sending.json"]
states = "crates/cli-report/src/report/send.rs::State"
variants = ["Pending", "Sent", "Kept"]
terminals = ["Sent", "Kept"]
```

- **Kind**: explicit (enum `State {Pending, Sent, Kept}`) plus `held: Option<Held>` (`Held::RepoChanged {current, refused}`), `attempts`, `next_at` and the gist and issue it made. Added after the audit, when the state inventory test found the file with no machine.
- **Source of truth**: `reports/<id>/sending.json`, one per report, written only by the CLI (`send.rs`, `write_sending`: temporary file and rename) under the report's `flock` (`.sending.lock`). The daemon reads it and never writes it: it runs the CLI.
- **Transitions**: none → `Pending` or `Sent` at the first `alondra report send` (`send_to`); `Pending` → `Sent` when GitHub takes it; `Pending` → `Pending` with `next_at = now + backoff_ms(attempts)` (one minute, doubling up to 30 min) when GitHub answers "later"; `Pending` → `Pending` held (`RepoChanged`, longest backoff) when a daemon retry finds another report repository; `Pending` → `Kept` with `alondra report keep`; a refusal (`Blocked`) leaves it `Pending` with `next_at = i64::MAX`. `Sent` and `Kept` are terminal; sending again a `Sent` report with the same package returns the same issue.
- **Who moves it**: the daemon's `retry_pending` loop (every `EVERY`) runs `alondra report send <id> --retry` for each `Pending` report whose `next_at` passed; a held report has a card (`report_held_card.rs`) whose "Send it" or "Keep it" runs the CLI, and a repository change in the settings retries it once (`nudged`).
- **Restart**: the file is the whole state; a daemon restart reads it again. A crash between GitHub's answer and the rename leaves the previous state, and the next try finds the gist and issue the earlier try left (`deliver` looks first), so no second issue is made.
- **Concurrency**: the CLI's `flock` serializes the user's send and the daemon's retry of one report.

### Findings (Rust part)

#### F-CLIENTS-R1 · The TUI automatic restart throws away answers not yet confirmed
**Class**: CONFIRMED · **Severity**: S2 · **Probability**: medium. The path is correlated: the TUI only restarts by itself when the daemon has updated, which is exactly when answers fail; releases are frequent · **Overlap**: none in open issues; `git log` of `updated.rs`/`optimistic.rs` shows no later fixes.
**Status**: FIXED in group G9 (issue #526), with the default decision D-R1. Still present on `main` (`e1d5ec2aa`): `busy()` ignores `optimistic` and `Restore` had no answers. `handover` puts every tab's unconfirmed answers in `Restore::unconfirmed`, `take_over` gives them back to the tab of the same Mac, due at once with their `command_id`, and their cards stay out; a manual quit over them asks first (`quits`). Tests (`//crates/cli-tui:cli_tui_test`): `updated::tests::answers_no_mac_confirmed_are_sent_again_by_the_restarted_terminal_with_their_commands` (the restore file goes through JSON, as on disk) and `updated::tests::quitting_with_an_answer_on_its_way_asks_first`. Ablation: with `handover` leaving `unconfirmed` empty the first fails (no answer due in the new process); with `quits` returning true at once the second fails; restored, both pass.
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
**Status**: FIXED in group G9 (issue #526), with a different fix from the one expected below. Mapping bare HTTP statuses the iroh way would undo a guarantee `main` keeps on purpose (tests `only_a_sealed_answer_speaks_for_the_mac_over_http`, `a_bare_status_from_a_route_the_mac_already_answered_on_is_no_answer_either`): over HTTP anyone who answers on the Mac's address can send a bare status, while iroh is an authenticated channel. So the Mac now seals to the device every refusal it can prove (`DirectRefusal::Sealed`, `service.rs`): `busy` when the request's signature holds (`busy_proof`, checked only once the permits are gone), `expired` for an envelope the device signed that arrived after its deadline, `idempotency_conflict` and `rejected`; the HTTP status stays the same. The terminal opens a non-200 body as a sealed refusal and reads it through the same `refused` as iroh (`direct_client.rs`): `busy` is `Unsent` (nothing ran), `expired` says the clocks differ, and the route is kept. A bare status and a refusal to an unknown or forged sender stay «no answer», so a terminal the Mac does not know at all still sees «Connecting…». Over iroh, `expired` replaces the `unauthorized` it got before (H-CLIENTS-R4 stays listed for pairing). Tests: `client-transport` `direct_client::iroh_tests::a_refusal_the_mac_sealed_reads_over_http_as_a_refusal`; `daemon-server` `direct::tests::a_refusal_to_a_device_that_proves_who_it_is_is_sealed_to_it_and_to_nobody_else`. Ablation: without the sealed-refusal branch in `over_http` the client test fails (`Unanswered`); with `sealed_refusal` returning `None` the server test fails (bare body); restored, both pass.
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
**Status**: FIXED in group G9 (issue #526). Every request for the screen's view, from `load_screen_view` or from the feed thread, takes a ticket from a counter both share (`view_feed::ticket`); `Msg::ViewHome` carries it and the TUI drops an answer, `Ok` or `Err`, to a request older than the newest one it took (`home_ticket`). The variant: an applied answer keeps the last ticket issued when the daemon settled it (`Applied::asked_before`), and a view asked for at or before it cannot bring its card back, also with no view on screen. Tests (`//crates/cli-tui:cli_tui_test`): `views_tests::a_screen_view_asked_for_earlier_never_replaces_a_newer_one_that_arrived_first`, `optimistic::tests::a_view_asked_for_before_the_daemon_confirmed_the_answer_cannot_bring_its_card_back`. Ablation: without the ticket check in `on_msg` the first fails (the answered card is listed again); without `asked_before` in `reconcile` the second fails; restored, both pass.
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
**Status**: FIXED in group G9 (issue #526), with the default decision D-R3. `LocalRoute::send` treats 401 `"device not paired"` like `"wrong signature"` (`Rejection`, `local.rs`): it pairs again once per process and sends the request again; the TUI shows once «This Mac no longer had this terminal among its devices; it paired again as “Terminal (…)”» (`Transport::local_notice`). With F-SERVER-D4 the Mac no longer lets anyone remove this terminal, so this covers a device removed before that or by hand. Test: `client-transport` `local::signing_tests::a_terminal_whose_device_was_removed_pairs_again_once_and_says_so`, which replaces `an_unpaired_device_is_not_paired_again` (it pinned the old behaviour). Ablation: with `Rejection::of` knowing only `"wrong signature"` the test fails (the request is refused and nothing pairs); restored, it passes.
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
A skew larger than the envelope deadline (10-70 s) rejects everything as `Unauthorized`; over iroh the message says "ya no acepta este terminal; vuelve a emparejar" (no longer accepts this terminal; pair again) (`direct_client.rs:208-216`, `419-423`), and pairing again does not fix it. Missing: confirming how the Mac computes `envelope_now` and whether pairing (120 s) also fails. Since G9 (F-CLIENTS-R2) an envelope the device signed that arrives after its deadline gets a sealed `expired` refusal and the terminal says the clocks differ, over HTTP and iroh; a terminal whose clock is ahead and pairing under skew are untouched, so the hypothesis stays listed.

### Part 2 · Apple apps (iPhone and Mac)

### What is already right

The Apple app is considerably better hardened than the TUI at the points where the Rust part found breakage:
- `FeedSession` already discards out-of-order view responses (owner and lifetime of each load, epoch generation; `apple/Data/Feed/Sources/FeedSession.swift:296-395`), so F-CLIENTS-R3 does not reproduce here.
- Each Mac has its own `Connection` (monitor, feed, answer sender, journal), so a late answer from the previous Mac cannot be applied to the new one (M-CLIENTS-A5).
- Account session: two concurrent requests share the same refresh, a network failure does not drop the session, and concurrent rotation cannot close the session (M-CLIENTS-A6, same as M-CLIENTS-R10 in the CLI).
- Direct pairing retries `URLError` until expiry (M-CLIENTS-A7).

### M-CLIENTS-A1 · Single connection state (`ConnectionMachine` / `ConnectionMonitor`)

```toml state-machine
id = "M-CLIENTS-A1"
code = ["apple/Contract/Sources/ConnectionState.swift::ConnectionMachine", "apple/Data/Feed/Sources/ConnectionMonitor.swift::ConnectionMonitor", "apple/Data/Account/Sources/Connection/ConnectionClassifier.swift::ConnectionClassifier"]
truth = ["memory"]
```

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

```toml state-machine
id = "M-CLIENTS-A2"
code = ["apple/Data/Feed/Sources/ViewTransport.swift::ViewActionSender", "apple/Data/Feed/Sources/ViewActionIntentStore.swift::ViewActionIntentStore", "apple/Data/Feed/Sources/ActionIntentDatabase.swift::ActionIntentDatabase"]
truth = ["memory"]
```

- **Kind**: composite. **Defined in**: memory: `pending: [commandId: Pending]`, `deliveries: [nonce: Task]`, `activeEpoch`, `journalEpoch/journalDescriptor`, withdrawal (`ViewTransport.swift:444-861`). Disk: `ViewActionIntentStore` / `ActionIntentDatabase` encrypted per pairing (`Connection.swift:57-59,110-114`; `apple/Data/Feed/Sources/ActionIntentDatabase.swift`, `ViewActionIntentStore.swift`) with per-record states (created/pending/completed/acknowledged) and quarantine of unreadable stores (`ActionIntentQuarantine.swift`). Plus the sealed-envelope journal per Mac (`EnvelopeJournal`, `Connection.swift:54-56`).
- **Copies and derivatives**: the daemon keeps a receipt per `command_id` and a journal of actions numbered by generation (`ActionOperationID`). Derived: `UndeliveredCards` (cards with an unconfirmed answer in the active epoch, `UndeliveredCards.swift:6-40`, `ViewTransport.swift:779-782`), which reads the queue to hide them after a relaunch.
- **States** per answer: prepared (numbered with the Mac journal generation, `prepare`, `:478-504`), then delivering (`keepDelivering`, loop with a 1...60 s pause or wake by `retryNow`, `:637-662`), then applied (`applied`: complete on disk and leaves `pending`, `:806-810`) | discarded (final error or epoch change, `discard`, `:773-777`) | re-issued with a new id if the seal expired and the Mac says it never finished it (`recoverOrResend`, `:752-771`).
- **Entries**: `prepare` + `deliver` (screens), `send` (legacy), `sendNow` (notification button: one attempt; if it does not arrive, leaves it in `pending` and raises `AnswerKeptForLater`, `:571-595`), `resumeDeliveries(epoch)` (relaunches those of that epoch, `:615-620`), `reconcilePending(epoch)` (asks the Mac about the pending ones, `:830-861`), `retryNow` (wakes the running deliveries, `:610-612`), `stop`.
- **Guards**: same intent gives the same `command_id` (`reserve`, `sameIntent`); one delivery per nonce (`startDelivery`, `:628-635`); epoch different from the active one gives discard (`:651-654`).
- **Persistence and restart**: intents survive relaunch and return to `pending` in `init` (`:463-468`); they are relaunched only from `resumeDeliveries`, which the feed reconciliation calls (`Connection.swift:222-225`, `FeedSession.swift:666-686`). Idempotency at the daemon by `command_id`/operation number.
- **Concurrency**: actor; deliveries are child `Task`s that return to the actor at each step.

### M-CLIENTS-A3 · Optimistic queue UI (`Optimistic` and `TriageFeature`, and `MacPresentationFeature`)

```toml state-machine
id = "M-CLIENTS-A3"
code = ["apple/Features/Sources/Triage/TriageFeature.swift::TriageFeature", "apple/Features/Sources/Workspace/MacPresentationFeature.swift::MacPresentationFeature", "apple/Data/Feed/Sources/UndeliveredCards.swift::UndeliveredCards"]
truth = ["memory"]
```

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

- **Terminals**: deleted. Invariant: every `sending` entry receives a `settled` or the epoch changes; since G10 a request that could not be prepared also emits `.settled(applied: false)` (F-CLIENTS-A5).
- **Other screens** (since G10): Tasks, the task detail and Scheduled tasks use the same `Optimistic` with a 30-second confirmation timer per sent action (`actionOverdue`): on expiry the row or button shows again with a toast that says why from the connection state (`LateNotice`) while delivery goes on (F-CLIENTS-A2).
- **Persistence and restart**: no; after a relaunch the hiding comes from `undelivered` (from disk).

### M-CLIENTS-A4 · State of an on-screen action (`ActionFeature`)

```toml state-machine
id = "M-CLIENTS-A4"
code = ["apple/Features/Sources/ViewsUI/ActionFeature.swift::ActionFeature"]
truth = ["memory"]
```

- **Kind**: implicit (flags): `pending`, `inFlight`, `stamp`, `stalled`, `unsure`, `error`, `changed`, `confirmation`, `pendingAttachmentSubmit` (`apple/Features/Sources/ViewsUI/ActionFeature.swift:14-53`).
- **Effective states**: idle; confirming; waiting for attachments; preparing (`inFlight && stamp == nil`); sent (`inFlight && stamp != nil`); stalled (`stalled`, at 10 s, `ViewsClient.swift:69`); answered (success with stamp, `changed` or `error`); doubtful (`unsure`: the same command is reused if repeated identically, `:438-452`).
- **Guards**: `present` does not overwrite a preparation in progress (`:190`); `prepared`/`answered`/`undelivered` only apply if the epoch and `commandId` match `pending` (`:288-330`); changing epoch clears everything (`didSet`, `:45-53`).
- **Effects**: `prepare` then `deliver` with `.cancellable(DeliveryID)`; the real delivery continues in the sender even if the screen goes away (`deliver` launches an unstructured `Task`, `:423-436`).

### M-CLIENTS-A5 · Paired Macs and selected Mac (`AccountSession` and `ApprovedSessionRegistry`)

```toml state-machine
id = "M-CLIENTS-A5"
code = ["apple/Data/Account/Sources/Composition/AccountSession.swift::AccountSession", "apple/Data/Pairing/Sources/ApprovedSessionRegistry.swift::ApprovedSessionRegistry"]
truth = ["memory"]
```

- **Kind**: composite. **Defined in**: `apple/Data/Account/Sources/Composition/AccountSession.swift` (`generation`, `selection`, `services`, `macs`, `choiceIDs`, `restoreTask`, `attempt`), `apple/Data/Pairing/Sources/ApprovedSessionRegistry.swift` (one live `Connection` per approved pairing and the selected one), pairings in the keychain (`PairingStore.swift`, `PairingCollection.swift`).
- **Source of truth**: keychain (collection and selection) plus the in-memory registry. **Copies and derivatives**: `services` published to the UI (`publishChoices`, `:596-610`); the scene is rebuilt by `Connection` identity (`AppScene.swift:31-32`, `.id(ObjectIdentifier(services))`).
- **Mac switch** (`selectMac`, `:570-628`): one selection at a time (`selection`); it checks that the stored keys match, does a signed round trip to the target Mac (`checkSettingsConnection`), persists the preference and calls `registry.select`; if something fails after persisting, it restores the live selection. `generation` invalidates everything started before a `login`/`logout`/`cancel`.
- **Isolation between Macs**: each `Connection` has its own monitor, feed, sender and intents scoped by `server/mac + device + credential revision` (`Connection.swift:50-59`). The TCA store and the `ViewsClient` are created per scene, bound to that `Connection` (`AppScene.swift:122-138`), so a late answer from the previous Mac only reaches the old store, which is discarded. No finding on this point.
- All connections stay alive in the background (`goLive` opens the iroh connection of all, `:369-372`); only the selected one has a feed (there is no other `viewsFeed` subscription).

### M-CLIENTS-A6 · Account session and token refresh (`AuthSession`)

```toml state-machine
id = "M-CLIENTS-A6"
code = ["apple/Data/Account/Sources/Auth/AuthSession.swift::AuthSession", "apple/Data/Account/Sources/Auth/RefreshCredential.swift::RefreshCredential"]
truth = ["memory"]
```

- **Kind**: explicit. **Defined in**: `apple/Data/Account/Sources/Auth/AuthSession.swift`. State in the keychain: `RefreshCredential{token, status: active | pendingRevocation | rotationUncertain | rejected}`; memory: `accessToken`, `expiresAt`, `refreshTask` (single flight), mutation lock (`reserveMutation`), `epoch`, `transitions`, `sessionSuspended`, `unpersistedCredential`.
- **Refresh** (`refresh`, `:400-449`): marks `rotationUncertain` on disk before sending; `invalid_grant` ends the session (`sessionLost`, `endRejectedSession`); another OAuth error goes back to `active`; a network failure without sending goes back to `active`; a lost response stays `rotationUncertain` and, with `refreshReplay = predecessorStaysValid` (`OIDCConfiguration.swift:41-42`), the next attempt repeats the stored token. One refresh at a time (`refreshTask` plus lock). 401/403 from userinfo or the relay: a refresh is forced at most once a minute and the session is kept (`:240-277`).
- **Classification**: `AuthError.rotationUncertain`/`pendingRevocation`/`noSession` give `sessionExpired` (asks to sign in), `oauth invalid_grant` gives `sessionExpired`; the rest are `accessNotConfirmed`/`serverUnreachable`/`unexpected` (passing) (`ConnectionClassifier.swift:83-94`). With the production configuration, `rotationUncertain` is never thrown (`activeCredential`, `:319-329`).

### M-CLIENTS-A7 · Pairing (QR through relay and direct on the local network)

```toml state-machine
id = "M-CLIENTS-A7"
code = ["apple/Data/Pairing/Sources/PairingClient.swift::PairingClient", "apple/Data/Pairing/Sources/Direct/DirectPairingClient.swift::DirectPairingClient"]
truth = ["memory"]
```

- **QR through relay**: `PairingClient` (`apple/Data/Pairing/Sources/PairingClient.swift`) with `attempts[UUID]` in memory: candidate sent (202), approval poll (204, and since G10 also 401, 408, 429, 5xx, a `URLError` other than cancelled, `admissionFull` or a 5xx bootstrap: wait 2 s and ask again; 200 verifies the signed proof and saves in the keychain; 404 or the QR's expiry: expired; other is an error), then saved (replaces the local pairing of the same Mac and selects it). The attempt keeps a token function (`PairingAuthorization.bearer`, the account's `bearerToken()`) and reports a refused token (`refused`, `accessTokenRefused(ifMatches:)`), so a token that expires while the owner types the code is renewed. Orchestrated by `AccountSession.begin/approve` with `PairingAttemptLease` (one attempt per scope), `generation` and the expected session. See F-CLIENTS-A3.

- **Direct**: `DirectPairingClient` (`apple/Data/Pairing/Sources/Direct/DirectPairingClient.swift:67-150`): hello, sealed open plus commitment, reveal, poll (retries `URLError` until expiry, unlike QR), verification of the signed approval against the transcript, then save. No findings.
- **Save**: `KeychainApprovedPairingStore` with collection and selection; changes with atomic `modify`.

### M-CLIENTS-A8 · Live Activity

```toml state-machine
id = "M-CLIENTS-A8"
code = ["apple/Data/LiveActivity/Sources/LiveActivityController.swift::LiveActivityController", "apple/Data/LiveActivity/Sources/ActivityRegistrar.swift::ActivityRegistrar"]
truth = ["memory"]
```

- **Defined in**: `LiveActivityController` (`apple/Data/LiveActivity/Sources/LiveActivityController.swift`).
- **State**: per Mac `{counts, live, heardAt, zeroSince}`; `knowsMacs`, `foreground`, `published`; serial `apply` queue (`tail`). Starts only in foreground with something pending; 15 min grace after reaching zero; expires at 30 min. Source: what the app loaded plus the sealed relay envelopes it reads from the activity itself. Since G10 a report from one Mac marks every other Mac not live: the app follows one Mac at a time, so the previous one keeps the time it was last loaded and a newer sealed report wins. Token registration at the relay: `ActivityRegistrar`. See F-CLIENTS-A4.

### M-CLIENTS-A9 · Notifications and push linking

```toml state-machine
id = "M-CLIENTS-A9"
code = ["apple/iOS/ViewsShipping/NotificationRuntime.swift::NotificationRuntime", "apple/Data/Notifications/Sources/Unregistration/PushUnregistrationLedger.swift::PushUnregistrationLedger"]
truth = ["memory"]
```

- **Defined in**: `NotificationRuntime.prepare` (`apple/iOS/ViewsShipping/NotificationRuntime.swift:51-112`): for each pairing with relay, one link, one decryption context and one `NotificationClient`; they are published together in a single main-thread commit with `generation`. `deactivate` clears keychain and notifications; pending unregistrations go to a ledger on disk (`PushUnregistrationLedger`). Notification buttons answer through `sendNow` (M-CLIENTS-A2, F-CLIENTS-A1); an answer kept for later starts its own background delivery, and a push wakes every paired Mac's sender (`AccountSession.macReached`) since G10. A normal tap selects the Mac of the push.


### M-CLIENTS-A10 · View loads and feed (`FeedSession`)

```toml state-machine
id = "M-CLIENTS-A10"
code = ["apple/Data/Feed/Sources/FeedSession.swift::FeedSession"]
truth = ["memory"]
```

- **Defined in**: `apple/Data/Feed/Sources/FeedSession.swift`. Per key, `loadOwners`/`loadLifetimes`/`inFlightLoads` with shared waits; `epochGeneration` invalidates loads of a previous epoch; the feed (`runFeed`) keeps cursor and versions; `reconnect()` restarts capabilities and the poll; `scheduleRecovery` reconciles actions by (generation, epoch). Interactive priority over background (4 at a time, 3 in background).
- **Rejected**: a response from a load that no longer owns the key is discarded or merged; one from another epoch is rejected. There is no on-disk home cache: covered by #386.

### Findings (Apple part)

#### F-CLIENTS-A1 · A notification answer that did not reach the Mac only goes out when that Mac's feed reconciles; with several Macs, the one for a non-selected Mac waits until the user switches to it
**Class**: CONFIRMED · **Severity**: S2 · **Probability**: low (two or more Macs, notification button, Mac unreachable at that moment) · **Overlap**: none in the open issues nor the plans of related work. `git log` of `ViewTransport.swift` shows no changes on this path.
**Status**: FIXED in group G10 (issue #527). An answer `sendNow` keeps for later starts its own background delivery at once (`startDelivery(_, failedSince:)` in `ViewTransport.swift`): it waits its turn and goes when anything wakes it (`retryNow`, a push, the network), with no screen or feed of that Mac open. A push wakes every paired Mac's sender (`AccountSession.macReached`, wired in `AppScene`), because the push callback does not say which Mac sent it. Test: `AlondraFeedTests` `NotificationAnswerTests.aKeptAnswerGoesOnceItsMacAnswersWithoutAnyScreenOfThatMac` (no `resumeDeliveries`, a retry policy that only wakes on `retryNow`). Ablation: without the `startDelivery` call in `sendNow` the test fails (`mac.runs` empty, the answer still unresolved); restored, it passes.
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
**Status**: FIXED in group G10 (issue #527). `TasksFeature`, `TaskDetailFeature` and `SchedulesFeature` start `Optimistic.confirmation` (30 s on the `continuousClock`) when an action is sent; on expiry `actionOverdue` calls `expire`, so the row or button shows again, and the toast says what has not reached the Mac and why, from the app's connection state (`LateNotice`, the user's decision of 24 Nov 2026: «No internet: still sending “Cancel Task”.»; es: «Sin internet: se sigue enviando «Cancelar tarea».»); the delivery goes on, and a late confirmation hides it again. The iPhone card queue uses the same notice. Each failed try logs its cause in the app log. Tests for the cause: `LateNoticeTests` (each connection state). Ablation: with `LateNotice.message` ignoring the connection, `LateNoticeTests.testAnUnreachableMacSaysWhyFirstAndThatItKeepsSending`, `testAConnectionStillStartingSaysSo` and the Tasks withdraw test fail; restored, they pass. `settle` cancels the timer. Tests: `AlondraFeaturesTests` `TasksFeatureTests.testAWithdrawTheMacNeverConfirmsShowsTheRequestAgainWithANoticeAfterTheTimeout`, `testAWithdrawTheMacConfirmsInTimeNeverComesBack`, `TaskDetailTests.testACancelTheMacNeverConfirmsShowsItsButtonAgainWithANoticeAfterTheTimeout` (`TestClock`, a `perform` held by a `Gate`); captures `TasksSnapshotTests.test_taskDetail_anUnconfirmedCancelShowsItsButtonAgainAndTheToastSaysWhich` in light and dark, English and Spanish. Ablation: with `Optimistic.confirmation` returning `.none` both «never confirms» tests fail (no `actionOverdue` arrives); restored, they pass.
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
**Status**: FIXED in group G10 (issue #527). `PairingAuthorization` holds a token function and a refusal callback instead of one token; `AccountSession.begin` passes the account's `bearerToken()` and `accessTokenRefused(ifMatches:)`, so the relay transport renews a refused or expiring token. `waitForApproval` waits `pollInterval` (2 s, injectable `sleep`) and asks again after a 401, 408, 429, 5xx, a `URLError` other than cancelled, `admissionFull` or a 5xx bootstrap, until the QR expires (`expired`). Tests: `AlondraPairingTests` `anAccessTokenThatExpiresWhileTheOwnerTypesTheCodeIsRenewedAndThePairingIsSaved` (the relay double answers 401 to the first token after 60 s and approves at 90 s), `aCutWhileWaitingForTheMacsApprovalIsAskedAgainInsteadOfGivingUp`, `aRelayThatKeepsRefusingStopsWhenTheQRExpires`, with an injected clock moved only by the poll's waits. Ablation: with the poll back to `204` only and no catch, the three tests fail (`unexpectedStatus(401)`, `URLError -1005`); restored, they pass. The orphan device on the Mac (evidence 5) no longer arises from these causes; a phone that is closed while waiting still leaves one until the sweep of unused devices, which is unchanged.
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
**Status**: FIXED in group G10 (issue #527). `LiveActivityController.report` marks every other Mac not live: the app follows one Mac at a time, so the Mac shown before keeps the time it was last loaded, a newer sealed report for it wins, and `busy` and the end of the activity follow the relay's numbers. Test: `AlondraLiveActivityTests` `LiveActivityControllerTests.aMacNoLongerShownNeverCoversWhatTheRelayLaterSaysOfIt` (the sequence above, then `background()`). Ablation: without the line in `report` the test fails (mac-a republished with the current time and the activity not ended); restored, it passes.
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
**Status**: FIXED in group G10 (issue #527). The iPhone queue half was already fixed on `main` by `77d49758b` (#533): `TriageFeature` gives the card back on `.actions(.undelivered)` (test `TriageFeatureTests.testAnAnswerThatCannotBeSavedOnThePhoneBringsTheCardBackAtOnceWithItsTextAndTheError`). The Mac sidebar still kept the card «sending» and said at 30 s that the Mac had not confirmed it. `ActionFeature.send` now emits `.settled(request, applied: false)` before `.undelivered` when preparing fails, so every screen ends its optimism; the Mac keeps the card out until the daemon's next sidebar, with the reason, as for any answer that did not go through. Tests: `AlondraFeaturesTests` `ActionPreparationTests.testFailedPreparationKeepsTheWrittenReplyWithoutClaimingItMayHaveArrived` (now requires the `.settled`), `AlondraMacServiceNavigationTests` `MacAnsweringTests.anAnswerThatCannotBePreparedIsNoLongerSendingAndNeverSaysTheMacDidNotConfirmIt`. Ablation: without the `.settled` both fail (the Mac test: still `isSending`, no reason, overdue after 30 s); restored, they pass.
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
