import AlondraDomain
import AlondraShared
import AlondraWorkspaceFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct TaskConnectionProjectionIntegrationTests {
    @Test func taskHeaderTracksAcceptedConnectionIndependentlyOfTaskSnapshots() async throws {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let overview = try decoder.decode(Overview.self, from: Data(#"{"tick":1,"nodes":[],"open_questions":[],"news":[],"orchestrator_messages":[],"machine":{"memory_total":1,"memory_available":1,"load":[0],"disk_available":1,"unreliable":false}}"#.utf8))
        let identity = TriageIdentity(server: URL(string: "https://service.example")!, deviceID: "phone", credentialRevision: "one", generation: 1)
        let other = TriageIdentity(server: identity.server, deviceID: "other", credentialRevision: "two", generation: 2)
        let store = Store(initialState: ServiceRootFeature.State()) { ServiceRootFeature() }
        await store.send(.triage(.triage(.inboxUpdated(.init(identity: identity, overview: overview, connection: .online))))).finish()
        await store.send(.tasks(.snapshot(overview))).finish()
        #expect(store.tasks.connection == .online)
        #expect(store.tasks.board.hasSnapshot)
        // A disconnected sync may retain its last task projection. The exact reason
        // still reaches Tasks; hasSnapshot must not be used as a connectivity test.
        await store.send(.triage(.triage(.inboxUpdated(.init(identity: identity, overview: overview, connection: .offline("Connection lost")))))).finish()
        #expect(store.tasks.connection == .offline("Connection lost"))
        #expect(store.tasks.board.hasSnapshot)
        await store.send(.tasks(.snapshot(nil))).finish()
        #expect(!store.tasks.board.hasSnapshot)
        #expect(store.tasks.connection == .offline("Connection lost"))
        await store.send(.triage(.triage(.inboxUpdated(.init(identity: other, overview: overview, connection: .online))))).finish()
        #expect(store.tasks.connection == .offline("Connection lost"))
        await store.send(.triage(.triage(.inboxUpdated(.init(identity: identity, overview: overview, connection: .online))))).finish()
        #expect(store.tasks.connection == .online)
        await store.send(.triage(.triage(.identityChanged(other)))).finish()
        #expect(store.tasks.connection == .connecting)
    }
}
