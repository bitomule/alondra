import AlondraDomain
import AlondraQuestionsFeature
import AlondraShared
import AlondraWorkspaceFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct FeedbackAuthorIntegrationTests {
    /// Line comments and the general note reach the delivery's one `submit_feedback`, from the
    /// card and from the document detail alike. The v2 projection names no user, so the comments
    /// carry no author.
    @Test(arguments: [false, true])
    func documentFeedbackReachesTheDeliveryAsOneCommand(detail: Bool) async throws {
        var fixture = V2Fixture()
        let root = fixture.documentReview("a", text: "Body")
        let delivery = root.children[0]
        let log = CommandLog()
        let store = Store(initialState: ServiceTriageFeature.State()) { ServiceTriageFeature() } withDependencies: {
            $0.domainWorkspace = .scripted(log, documents: ["rev-a": "Body"]) { _ in V2Fixture.rejected() }
        }
        await store.send(.triage(.workspaceUpdated(fixture.update()))).finish()
        try #require(store.cards[id: root.id]?.document != nil)
        if detail { await store.send(.open(root.id)).finish() }
        // Another session's update must not replace this one's cards.
        await store.send(.triage(.workspaceUpdated(V2Fixture().update(V2Fixture.other)))).finish()
        func documentAction(_ action: DocumentReviewFeature.Action) -> ServiceTriageFeature.Action {
            detail ? .documents(.element(id: delivery.id, action: action)) : .cards(.element(id: root.id, action: .document(action)))
        }
        await store.send(documentAction(.appeared)).finish()
        await store.send(documentAction(.generalChanged("Retain this"))).finish()
        await store.send(documentAction(.commentChanged(anchor: "block-1", quote: "Body", text: "Change spacing"))).finish()
        if detail {
            let author = try #require(store.document?.feedbackAuthor)
            #expect(author.isEmpty)
            await store.send(documentAction(.sendFeedbackTapped(author: author))).finish()
        } else {
            #expect(store.cards[id: root.id]?.document?.feedbackAuthor.isEmpty == true)
            await store.send(.cards(.element(id: root.id, action: .send))).finish()
        }
        #expect(log.count == 1)
        guard case let .submitFeedback(id, text, _) = try #require(log.intents.first) else {
            Issue.record("Feedback is one submit_feedback: \(log.intents)"); return
        }
        #expect(id == "delivery-a")
        #expect(text.hasPrefix("Comments:"))
        #expect(text.contains("Change spacing") && text.contains("Retain this"))
        await store.send(.triage(.identityChanged(V2Fixture.other))).finish()
        #expect(store.feedbackAuthor.isEmpty)
        #expect(store.cards.isEmpty && store.document == nil)
    }
}
