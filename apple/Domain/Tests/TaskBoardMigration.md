# TaskBoard test migration

The old Shared suite mixed decoded contracts, board rules, localized presentation,
network loading and FleetStore state. Its replacement has no FleetStore dependency.

- Domain/Tests/TaskBoardTests.swift preserves contract decoding and board structure,
  dependency ordering, probe filtering, archival and history-key assertions. Empty
  overview checks now construct TaskBoard directly, with the same decoded inputs.
- Shared/Tests/TaskBoardTests.swift retains the three tests of localized task labels,
  fleet row titles and PR launch-option labels.
- Demo/Tests/TaskPresentationIntegrationTests.swift uses ServiceTaskPresentation for
  task/card association, project, running/waiting text, awaited PR explanation, request
  title priority and documents supplied by the task detail rather than the overview.
  Expected strings and decoded fixtures are preserved. ServiceTasksFeature additionally
  verifies that a finished request cannot dispatch withdrawal or publish its result.
- FleetFeatureTests.openThreadsFollowSnapshotTicksAndPeriodicToolChanges replaces
  polling through FleetStore with NodeDetailFeature, real operations and TestClock.
  It preserves the same-tick/tool-change distinction and visible periodic updates.
- FleetFeatureTests.aFailedThreadReadKeepsTheLastLedgerAndOnlyFallsBackBeforeItsFirstSuccess
  preserves overview fallback before the first ledger response and the last successful
  ledger across later failures, using the current feature's polling effect.

Two obsolete behaviors are deliberately not recreated:

1. Retrying the full node detail route after an old daemon lacks `/activity` is not a
   supported contract of the single approved service. The current service adapter uses
   its activity endpoint. Light/wrapped NodeThreads decoding is still checked, without
   retaining a local-daemon transport compatibility path for this old test.
2. A detail from another historyKey is stale. TaskFeatureTests.
   newHistoryKeyDropsOldDetailWhileNewDetailsLoad already verifies it is removed before
   loading the new revision and stays absent on failure. The old assertion that kept
   an earlier revision is superseded by that existing guarantee, not silently treated
   as equivalent. DTO/history-key assertions remain in Domain.

This change neither edits SyncSession tests nor changes production loading behavior.
Whether a snapshot tick should also trigger an immediate node activity refresh is a
separate behavior review; these transferred checks retain the periodic-update guarantee.
