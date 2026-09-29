import AlondraDomain
import AlondraQuestionsFeature
import AlondraShared
import AlondraWorkspaceFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct CardReviewReadinessTests {
    private func documentCard() throws -> TriageCardFeature.State {
        var fixture = V2Fixture()
        let root = fixture.documentReview("a")
        var card = TriageCardFeature.State(item: root, workspace: fixture.workspace, identity: V2Fixture.workIdentity)
        try #require(card.document != nil)
        card.document?.draft.general = "Please shorten it"
        return card
    }

    @Test
    func sendStaysOffWhileTheDocumentIsLoadingOrFailed() async throws {
        var loading = try documentCard()
        loading.document?.isLoading = true
        #expect(!loading.reviewReady && !loading.canSend)

        var failed = try documentCard()
        failed.document?.loadError = "The document could not be read"
        #expect(!failed.reviewReady && !failed.canSend)

        let log = CommandLog()
        let store = Store(initialState: failed) { TriageCardFeature() } withDependencies: {
            $0.domainWorkspace = .scripted(log) { _ in V2Fixture.recorded }
        }
        await store.send(.send).finish()
        #expect(store.pendingStamp == nil)
        #expect(log.count == 0)

        var loaded = try documentCard()
        loaded.document?.sourceText = "Body"
        #expect(loaded.reviewReady && loaded.canSend)
    }

    @Test
    func aDocumentReviewRootOpensItsPendingDelivery() throws {
        var fixture = V2Fixture()
        var root = fixture.documentReview("a")
        let pending = root.children[0]
        var settled = pending
        settled.id = "interaction:earlier"
        settled.pending = false
        root.children = [settled, pending]
        fixture.inbox = [root]
        guard case .document(let subject) = CardContent(root, in: fixture.workspace) else {
            Issue.record("A document review root opens as a document"); return
        }
        #expect(subject.item.id == pending.id, "The pending delivery opens, not an earlier settled one")
        #expect(subject.isDelivery)
        #expect(subject.revisions.map(\.revision.id.rawValue) == ["rev-a"])
        #expect(subject.action(.acceptDelivery) != nil && subject.action(.submitFeedback) != nil)
    }
}
