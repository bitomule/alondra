# Mac service integration

The active app creates a ServiceRootFeature store per window. CloudAppServices owns
the shared service transport and SyncSession; MacPresentationFeature owns native
window selection. MacAppSync, MacLegacyProjection, LocalDevice and mutable ReviewDraft
are removed. Live presentation components were moved unchanged before that removal.

MacTriageIntegrationTests now drives two actual service root stores with the same
decoded projection, tests independent cursor/navigation, identity invalidation and
rejection of late projections. It does not claim to test SwiftUI's onChange wiring
or actual subscriptions: MacServiceNavigationTests checks navigation and SyncSession
integration tests separately verify two subscribers share one poll and lifecycle
cancellation. TriageFeatureTests.identitySwitchClearsQueueAndIgnoresLateMutationAndSnapshot
and Commands/Recovery integration suites own stale-command rejection.

The disconnected MacWindowFeature has been removed. Its coverage inventory is in
`docs/testing/retired-root-feature-coverage.md`. Builds and executed tests for this unit
are pending the shared Mac compilation slot.
