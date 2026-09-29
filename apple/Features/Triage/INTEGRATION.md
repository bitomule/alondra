# F1 triaje: contrato para H y G1/G2

`//apple/Features/Triage:AlondraTriageFeature` owns queue, cursor, optimistic hide,
pending mutations, selected detail ID and identity invalidation. It depends on
`AlondraShared` only for the existing immutable server models, `InboxGrouping` and
`InboxRing`; step I may move those pure types out of the legacy shared module.

## Scene wiring

Create one `StoreOf<TriageFeature>` per iOS scene or Mac window. Install
`DependencyValues.triageService` when creating the store. Its `observe` closure
subscribes to the **existing app-owned** `SyncSession`; it never creates another
transport or poll. Decode only the current `Overview` to `TriageInboxUpdate`.
Passing `overview: nil` indicates a temporary disconnection and retains the last
cards while setting `hasSnapshot = false`.

Before a URL, device or credential change, synchronously send
`.identityChanged(TriageIdentity(server:deviceID:credentialRevision:generation:))`.
That clears queue, hidden state, pending IDs and selected detail. Then switch the
process SyncSession/CommandGate. Delayed updates and mutation results from the old
identity are ignored. Generation must increase on a credential rotation even if
server and device ID are unchanged. `observe` emits updates with that exact
identity. Cancel the feature's subscription with `.disappeared` when its scene or
window goes away; `.appeared` uses its own cancellation ID.

For `.mutate(kind,itemID,identity)`, the adapter must verify the **current**
CommandScope matches `identity`, then call the app-owned `CommandGate.submit` with
the corresponding typed `AlondraClient` operation. Map `.confirmed`, `.rejected`,
`.unknown`, `.obsolete` to `TriageMutationOutcome`. Never replay an `.unknown`
mutation. The reducer restores the card for rejected/unknown results and shows a
notice; confirmed cards stay hidden until a snapshot **received after confirmation**
omits them, including when the card was temporarily absent at confirmation time. Only notices
the server offers `mark_read` for accept `.hide`; questions, deliveries and pull requests leave
through their own decisions.

## Views and detail features

`//apple/Features/TriagePresentation:AlondraTriagePresentation` supplies
`TriageDeck` on iOS and `TriageSidebarRows` on Mac. H supplies the existing
`TriageCard`/`InboxRow` through their view-builder closures, preserving visual
content, localization and accessibility. The deck passes review, skip and hide
action closures to the card; H must replace the card's remaining FleetStore
mutations with those closures or G1/G2 detail actions. The presentation does not
own the queue or call a client. When `hasSnapshot == false` and the queue is empty,
the adapters show "Waiting for the server"; the root can refine the connection
message, but must never render "All caught up" before a snapshot arrives.
`store.presentedItem` gives H the selected `InboxItem`; pass it to G1/G2 detail
features. Send `.dismissReview` when the detail closes. If an item disappears
from an overview, the reducer dismisses its detail.

The `TriageInbox` projection and `TriageFeature.State` expose
`questionsByPullRequest` and `reportsByPullRequest` (and state helpers
`questions(forPullRequest:)` / `reports(forPullRequest:)`). G1/G2 must read those
children when showing an open PR card; they are deliberately omitted from the
top-level queue and deduplicated within their PR.

`ObserveInboxUseCase.execute(Overview)` is synchronous, pure and usable from
either platform. It filters orchestrator-only items, groups questions/reports
inside open PR cards, deduplicates IDs, sorts the ring and computes counts.
One server item therefore appears once in a scene's card queue.

This PR deliberately leaves `apple/BUILD.bazel`, app roots and `FleetStore` for
H. The existing app screens continue to work until H connects the new targets.

## Verification

`bazelisk test //apple/Features/Triage:AlondraTriageFeatureTests` exercises
deduplication/grouping including PR children, optimistic rollback, old-identity
rejection, a temporarily omitted in-flight command, and confirmation during the
omission followed by stale reappearance. Pending IDs are held until completion
or identity reset even if an intermediate snapshot omits them.
`bazelisk build //apple/Features/TriagePresentation:AlondraTriagePresentation`
checks the Mac SwiftUI adapter. H must build the signed iOS and Mac applications
after adding the presentation target to their entry dependencies, and use MAV for
end-to-end UI validation.
