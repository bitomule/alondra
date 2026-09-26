import AlondraDomain
import AlondraQuestionsFeature
import AlondraShared
import AlondraWorkspaceFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct FeedbackAuthorIntegrationTests {
    @Test(arguments: [false, true], ["", "Ana"])
    func acceptedOverviewAuthorReachesActualFeedback(detail: Bool, author: String) async throws {
        let identity = TriageIdentity(server: URL(string: "https://service.example")!, deviceID: "phone", credentialRevision: "one", generation: 1)
        let other = TriageIdentity(server: identity.server, deviceID: "other", credentialRevision: "two", generation: 2)
        func snapshot(_ user: String?) throws -> Overview {
            let decoder = JSONDecoder()
            decoder.keyDecodingStrategy = .convertFromSnakeCase
            let userField = user.map { ",\"user\":\"\($0)\"" } ?? ""
            return try decoder.decode(Overview.self, from: Data("""
            {"tick":1,"nodes":[],"open_questions":[],"news":[{"id":7,"session_id":"s","title":"Report","body":"Body","links":[{"url":"https://claude.ai/artifact/review-1"}],"attachments":[],"kind":"document","created_at":1}],"orchestrator_messages":[],"machine":{"memory_total":1,"memory_available":1,"load":[0],"disk_available":1,"unreliable":false}\(userField)}
            """.utf8))
        }
        let sent = LockIsolated<[String]>([])
        let store = Store(initialState: ServiceTriageFeature.State()) { ServiceTriageFeature() } withDependencies: {
            $0.questionWorkService = .init(submit: { _ in .failed("Unexpected route") }, submitReportFeedback: { request, _ in
                sent.withValue { $0.append(request.text) }
                return .failed("Rejected")
            }, document: { _ in "Body" })
        }
        await store.send(.triage(.inboxUpdated(.init(identity: identity, overview: try snapshot("Previous"), connection: .online)))).finish()
        let id = try #require(store.cards.first?.id)
        if detail { await store.send(.open(id)).finish() }
        await store.send(.triage(.inboxUpdated(.init(identity: identity, overview: try snapshot(author.isEmpty ? nil : author), connection: .online)))).finish()
        // An old/foreign stream must not replace the accepted author's projection.
        await store.send(.triage(.inboxUpdated(.init(identity: other, overview: try snapshot("Wrong"), connection: .online)))).finish()
        func documentAction(_ action: DocumentReviewFeature.Action) -> ServiceTriageFeature.Action {
            detail ? .document(.presented(action)) : .cards(.element(id: id, action: .document(action)))
        }
        await store.send(documentAction(.generalChanged("Retain this"))).finish()
        await store.send(documentAction(.commentChanged(anchor: "block-1", quote: "Body", text: "Change spacing"))).finish()
        if detail {
            let projected = try #require(store.document?.feedbackAuthor)
            #expect(projected == author)
            await store.send(documentAction(.submitFeedback(author: projected))).finish()
        } else {
            #expect(store.cards[id: id]?.document?.feedbackAuthor == author)
            await store.send(.cards(.element(id: id, action: .send))).finish()
        }
        #expect(sent.value.count == 1)
        let text = try #require(sent.value.first)
        #expect(text.hasPrefix(author.isEmpty ? "Comments:" : "Comments from Ana:"))
        #expect(text.contains("Change spacing") && text.contains("Retain this"))
        #expect(!text.contains("David") && !text.contains("Previous") && !text.contains("Wrong"))
        await store.send(.triage(.identityChanged(other))).finish()
        #expect(store.feedbackAuthor.isEmpty)
        #expect(store.cards.isEmpty && store.document == nil)
    }
}
