# macOS root integration

This branch is the E-Mac root step. It depends on D1b (#133) and must land after it.

## Bazel integration

`apple/BUILD.bazel` belongs to the train integrator. Add these direct dependencies to
`AlondraMacEntry` when merging this branch:

```text
//apple/APIClient:AlondraAPIClient
//apple/Data/Commands:AlondraCommands
//apple/Data/Sync:AlondraSync
//apple/macOS/Feature:AlondraMacFeature
@swiftpkg_swift_composable_architecture//:ComposableArchitecture
```

The existing `AlondraShared` and `AlondraDeviceIdentity` dependencies remain.

## Ownership and transition

- `MacAppSync` is created once by `AlondraMacApp`. It owns one `SyncSession`, one
  `CommandGate`, one URLSession transport and one activity monitor for the process.
- Each `WindowGroup` instance creates a `MacWindowContext` with its own TCA Store,
  selection, search query, presentation state and review drafts. Closing a window
  cancels only its subscription; another window keeps the shared poll alive.
- `MacLegacyProjection` is the short-lived bridge for screens still backed by
  `FleetStore`. It decodes overview away from the main actor, checks identity and
  generation on the main actor, then applies the snapshot. It never calls
  `FleetStore.run()` or opens another long poll.
- Legacy local hide/resolve changes emit `localInboxChanged` to the window
  reducer, so selection advances immediately while the server snapshot catches up.
- On key rotation or server change, `MacAppSync.prepareUse` increments the shared
  command scope generation synchronously. `LocalDevice` calls it before yielding
  after pairing, and it invalidates every registered window before the async
  `CommandGate.use` call. Each window clears its old draft and presentation state;
  then `activate` switches the gate and sync session.
- Subscription cancellation IDs include the window UUID. TCA stores cancellation
  IDs globally, so opening or closing another window cannot cancel this one.
- Pairing errors are produced by the process service and projected to each
  window's TCA state for presentation.
- An identity change dismisses the new-request sheet and recreates the window's
  local SwiftUI presentation, clearing old smart-search state. A submission
  adapter captures the sheet's original identity, presentation epoch and command
  scope. The reducer expects the next identity and generation before accepting
  a stream event, so a queued old event cannot restore the previous sheet or
  authorize its send. F5 will move the whole form to TCA.
- D1b's `CommandGate` prevents automatic mutation replay and rejects results from
  an obsolete scope. It does not deduplicate two separate user taps; backend
  idempotency remains a separate wire-contract dependency.
- Legacy buttons capture the app command scope at the user gesture. Their async
  tasks check it before entering `FleetStore`, preventing a queued action from
  reading the new client after a server or credential switch.

## Follow-up feature work

The existing detail screens still use `FleetStore` for mutations and local
presentation. F1–F5 and H should move their state and effects into TCA features and
use cases, then remove `MacLegacyProjection`, `FleetStore` from the Mac target and
the remaining direct client calls from SwiftUI views. In particular, migrate the
sidebar's smart search, archive/close actions, detail drafts and pairing error
presentation. The final audit must verify no view owns API or business rules.

## Evidence

`bazelisk test //apple/macOS/Feature:AlondraMacFeatureTests` covers window
subscription cancellation, selection, a queued old event across an identity
switch, an old sheet's send eligibility and a stale decoded response. The full
`AlondraMacEntry` compiles with the Bazel dependencies above applied temporarily.
