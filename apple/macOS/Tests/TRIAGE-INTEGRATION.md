# Mac triage integration

Each `MacWindowContext` owns a `TriageFeature` store. `MacAppSync` supplies both
window stores from its existing process-owned `SyncSession`; the triage observer
is intentionally unused, so opening a window adds no poll. Identity changes
invalidate all window stores synchronously before switching the shared session.

The sidebar, counts, inbox order, PR question badges and triage context actions
read TCA state. Read/dismiss actions use the shared `CommandGate`, capturing the
client and command scope before I/O. An obsolete identity cannot submit.
Skip updates the triage cursor and sends navigation actions to the window reducer.
SwiftUI supplies the native New Window command through the File menu and Command-N;
the previous empty replacement command group is removed. New Request uses Shift-Command-N,
so its shortcut does not compete with the native window command.

## Meaningful verification

`//apple:AlondraMacTriageIntegrationTests` drives the actual process service and
two window contexts: both receive a decoded snapshot, one changes selection and
cursor without affecting the other, a server switch clears both windows, a queued
old command is rejected and an old snapshot cannot repopulate either window.
`//apple/macOS/Feature:AlondraMacFeatureTests` covers subscription cancellation and
window navigation. `//apple:AlondraMac` builds the actual app.

## Remaining adapters for subsequent PRs

This is the inbox integration step, not the final architecture gate. Detail
screens (`ItemDetail`, `QuestionDetail`, `PullRequestDetail`, `ReportDetail`,
`TaskDetail`), scheduled tasks, new requests, archive/close and smart search still
use legacy adapters. Their state and service calls must move to the corresponding
feature modules before removing `FleetStore` and `MacLegacyProjection`. Legacy
detail actions reconcile with this inbox on the next shared server projection.
The final audit must verify that no SwiftUI view performs I/O or owns business
rules. This step does not prove end-to-end read/dismiss delivery on a test server;
it deliberately does not mutate live user inbox items during MAV verification.

The final transport destination is a single service APIClient for iOS and Mac.
The direct daemon client used by this transitional adapter must be removed, along
with FleetStore, before the migration can be considered complete.
