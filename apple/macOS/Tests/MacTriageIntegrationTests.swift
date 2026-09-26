import AlondraShared
import AlondraWorkspaceFeature
import AlondraTriageFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct MacTriageIntegrationTests {
    private func snapshot(_ ids: [Int]) throws -> Overview {
        let questions = ids.map { id in
            #"{"id":\#(id),"sessionId":"node","title":"Question \#(id)","options":[],"links":[],"attachments":[],"origin":"ask","status":"open","askedAt":\#(id)}"#
        }.joined(separator: ",")
        let json = #"{"tick":1,"nodes":[],"open_questions":[\#(questions)],"news":[],"orchestrator_messages":[],"machine":{"memory_total":1,"memory_available":1,"load":[0],"disk_available":1,"unreliable":false},"prs":[]}"#
        let body = Data(json.utf8)
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(Overview.self, from: body)
    }

    @Test
    func sharedProjectionKeepsWindowCursorAndNavigationIndependentAndRejectsOldIdentity() async throws {
        let first = Store(initialState: ServiceRootFeature.State()) { ServiceRootFeature() }
        let second = Store(initialState: ServiceRootFeature.State()) { ServiceRootFeature() }
        let firstNavigation = first.scope(state: \.macPresentation, action: \.macPresentation)
        let secondNavigation = second.scope(state: \.macPresentation, action: \.macPresentation)
        let server = try #require(URL(string: "https://service.example.test"))
        let old = TriageIdentity(server: server, deviceID: "mac", credentialRevision: "old", generation: 0)
        let replacement = TriageIdentity(server: server, deviceID: "mac", credentialRevision: "new", generation: 1)
        let update = TriageInboxUpdate(identity: old, overview: try snapshot([1, 2]), connection: .online)
        for store in [first, second] {
            await store.send(.triage(.triage(.identityChanged(old)))).finish()
            await store.send(.triage(.triage(.inboxUpdated(update)))).finish()
        }
        let initialIDs = first.triage.triage.queue.map(\.id)
        #expect(Set(initialIDs) == ["q1", "q2"])
        #expect(second.triage.triage.queue.map(\.id) == initialIDs)
        for navigation in [firstNavigation, secondNavigation] { navigation.send(.inboxChanged(initialIDs)) }
        firstNavigation.send(.select(.item("q2")))
        secondNavigation.send(.select(.node("worker")))
        #expect(firstNavigation.selection == .item("q2"))
        #expect(secondNavigation.selection == .node("worker"))
        let front = try #require(initialIDs.first)
        await first.send(.triage(.triage(.skip(front)))).finish()
        #expect(first.triage.triage.queue.map(\.id) == Array(initialIDs.dropFirst()) + [front])
        #expect(second.triage.triage.queue.map(\.id) == initialIDs)
        #expect(first.triage.triage.identity == second.triage.triage.identity)

        for store in [first, second] {
            await store.send(.triage(.triage(.identityChanged(replacement)))).finish()
            #expect(store.triage.triage.queue.isEmpty)
            #expect(store.triage.cards.isEmpty)
            await store.send(.triage(.triage(.inboxUpdated(update)))).finish()
            #expect(store.triage.triage.queue.isEmpty, "An old projection cannot repopulate the new identity")
            let next = TriageInboxUpdate(identity: replacement, overview: try snapshot([3]), connection: .online)
            await store.send(.triage(.triage(.inboxUpdated(next)))).finish()
            #expect(store.triage.triage.queue.map(\.id) == ["q3"])
            #expect(store.triage.cards.map(\.id) == ["q3"])
        }
    }
}
