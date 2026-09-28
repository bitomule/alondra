import AlondraDomain
import AlondraQuestionsFeature
import AlondraShared
import AlondraWorkspaceFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct CardReviewReadinessTests {
    private let identity = QuestionWorkIdentity(server: URL(string: "https://service.example")!, deviceID: "phone", credentialRevision: "one", generation: 1)

    private func reportCard() throws -> TriageCardFeature.State {
        let report = try JSONDecoder().decode(Report.self, from: Data(#"{"id":7,"sessionId":"s","title":"Report","body":"Body","links":[{"url":"https://claude.ai/artifact/review-1"}],"attachments":[],"kind":"document","createdAt":1}"#.utf8))
        var card = TriageCardFeature.State(item: .report(report), identity: identity)
        card.document?.draft.general = "Please shorten it"
        return card
    }

    @Test
    func sendStaysOffWhileTheDocumentIsLoadingOrFailed() async throws {
        var loading = try reportCard()
        loading.document?.isLoading = true
        #expect(!loading.reviewReady && !loading.canSend)

        var failed = try reportCard()
        failed.document?.loadError = "The document could not be read"
        #expect(!failed.reviewReady && !failed.canSend)

        let store = TestStore(initialState: failed) { TriageCardFeature() }
        await store.send(.send)
        #expect(store.state.pendingStamp == nil)

        var loaded = try reportCard()
        loaded.document?.sourceText = "Body"
        #expect(loaded.reviewReady && loaded.canSend)
    }
}
