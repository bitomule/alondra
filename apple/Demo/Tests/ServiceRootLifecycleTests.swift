import AlondraShared
import AlondraTriageFeature
import AlondraWorkspaceFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct ServiceRootLifecycleTests {
    @Test(.timeLimit(.minutes(1)))
    func reconnectKeepsSelectedTabAndClosingOneSceneLeavesTheOtherSubscribed() async throws {
        let identity = TriageIdentity(server: URL(string: "https://service.example")!,
            deviceID: "phone", credentialRevision: "approved", generation: 1)
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let overview = try decoder.decode(Overview.self, from: Data(#"{"tick":1,"nodes":[],"open_questions":[],"news":[],"orchestrator_messages":[],"machine":{"memory_total":1,"memory_available":1,"load":[0],"disk_available":1,"unreliable":false}}"#.utf8))
        let a = AsyncStream<TriageInboxUpdate>.makeStream()
        let b = AsyncStream<TriageInboxUpdate>.makeStream()
        let first = TestStore(initialState: ServiceRootFeature.State()) { ServiceRootFeature() } withDependencies: {
            $0.triageService = TriageService(observe: { a.stream }, mutate: { _, _, _ in .obsolete })
        }
        let second = TestStore(initialState: ServiceRootFeature.State()) { ServiceRootFeature() } withDependencies: {
            $0.triageService = TriageService(observe: { b.stream }, mutate: { _, _, _ in .obsolete })
        }
        // Cross-feature integration: child projection effects are consumed without asserting
        // every internal write. The observable connection, snapshot and selected tab are
        // checked after each stage, and both real subscription effects must finish below.
        first.exhaustivity = .off(showSkippedAssertions: true)
        second.exhaustivity = .off(showSkippedAssertions: true)
        await first.send(.triage(.triage(.appeared)))
        await second.send(.triage(.triage(.appeared)))
        let online = TriageInboxUpdate(identity: identity, overview: overview, connection: .online)
        a.continuation.yield(online)
        b.continuation.yield(online)
        await first.receive(\.triage.triage.inboxUpdated)
        await second.receive(\.triage.triage.inboxUpdated)
        await first.send(.tabSelected(.tasks))
        await second.send(.tabSelected(.fleet))
        #expect(first.state.triage.triage.connection == .online)
        #expect(second.state.triage.triage.connection == .online)

        a.continuation.yield(TriageInboxUpdate(identity: identity, overview: overview, connection: .offline("temporary")))
        await first.receive(\.triage.triage.inboxUpdated)
        #expect(first.state.tab == .tasks)
        #expect(first.state.triage.triage.connection == .offline("temporary"))
        #expect(first.state.triage.triage.hasSnapshot)
        a.continuation.yield(online)
        await first.receive(\.triage.triage.inboxUpdated)
        #expect(first.state.tab == .tasks)
        #expect(first.state.triage.triage.connection == .online)
        #expect(first.state.triage.triage.hasSnapshot)

        await first.send(.triage(.triage(.disappeared)))
        await first.finish() // Finish before closing the producer: cancellation must own this.
        b.continuation.yield(TriageInboxUpdate(identity: identity, overview: overview, connection: .offline("second scene")))
        await second.receive(\.triage.triage.inboxUpdated)
        #expect(second.state.tab == .fleet)
        #expect(second.state.triage.triage.connection == .offline("second scene"))
        #expect(second.state.triage.triage.hasSnapshot)
        await second.send(.triage(.triage(.disappeared)))
        await second.finish()
        a.continuation.finish()
        b.continuation.finish()
    }
}
