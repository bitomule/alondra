import AlondraShared
import AlondraTriageFeature
import AlondraWorkspaceFeature
import ComposableArchitecture
import Foundation
import Synchronization
import Testing

@MainActor
struct ServiceRootLifecycleTests {
    @Test
    func reconnectKeepsSelectedTabAndSnapshot() async throws {
        let identity = TriageIdentity(server: URL(string: "https://service.example")!,
            deviceID: "phone", credentialRevision: "approved", generation: 1)
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let overview = try decoder.decode(Overview.self, from: Data(#"{"tick":1,"nodes":[],"open_questions":[],"news":[],"orchestrator_messages":[],"machine":{"memory_total":1,"memory_available":1,"load":[0],"disk_available":1,"unreliable":false}}"#.utf8))
        let store = Store(initialState: ServiceRootFeature.State()) { ServiceRootFeature() }
        let online = TriageInboxUpdate(identity: identity, overview: overview, connection: .online)
        await store.send(.triage(.triage(.inboxUpdated(online)))).finish()
        await store.send(.tabSelected(.tasks)).finish()
        #expect(store.triage.triage.connection == .online)
        await store.send(.triage(.triage(.inboxUpdated(TriageInboxUpdate(
            identity: identity, overview: overview, connection: .offline("temporary")
        ))))).finish()
        #expect(store.tab == .tasks)
        #expect(store.triage.triage.connection == .offline("temporary"))
        #expect(store.triage.triage.hasSnapshot)
        await store.send(.triage(.triage(.inboxUpdated(online)))).finish()
        #expect(store.tab == .tasks)
        #expect(store.triage.triage.connection == .online)
        #expect(store.triage.triage.hasSnapshot)
    }

    @Test(.timeLimit(.minutes(1)))
    func closingOneSceneLeavesTheOtherSubscribed() async throws {
        let identity = TriageIdentity(server: URL(string: "https://service.example")!,
            deviceID: "phone", credentialRevision: "approved", generation: 1)
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let overview = try decoder.decode(Overview.self, from: Data(#"{"tick":1,"nodes":[],"open_questions":[],"news":[],"orchestrator_messages":[],"machine":{"memory_total":1,"memory_available":1,"load":[0],"disk_available":1,"unreliable":false}}"#.utf8))
        let a = AsyncStream<TriageInboxUpdate>.makeStream()
        let b = AsyncStream<TriageInboxUpdate>.makeStream()
        let terminations = StreamTerminations()
        a.continuation.onTermination = { reason in
            terminations.record("a", reason: reason)
        }
        b.continuation.onTermination = { reason in
            terminations.record("b", reason: reason)
        }
        let first = TestStore(initialState: ServiceTriageFeature.State()) { ServiceTriageFeature() } withDependencies: {
            $0.triageService = TriageService(observe: { a.stream }, mutate: { _, _, _ in .obsolete })
        }
        let second = TestStore(initialState: ServiceTriageFeature.State()) { ServiceTriageFeature() } withDependencies: {
            $0.triageService = TriageService(observe: { b.stream }, mutate: { _, _, _ in .obsolete })
        }
        // Cross-feature integration: child projection effects are consumed without asserting
        // every internal write. The observable connection and snapshot are
        // checked after each stage, and both real subscription effects must finish below.
        first.exhaustivity = .off(showSkippedAssertions: true)
        second.exhaustivity = .off(showSkippedAssertions: true)
        await first.send(.triage(.appeared))
        await second.send(.triage(.appeared))
        let online = TriageInboxUpdate(identity: identity, overview: overview, connection: .online)
        a.continuation.yield(online)
        b.continuation.yield(online)
        await first.receive(\.triage.inboxUpdated)
        await second.receive(\.triage.inboxUpdated)
        #expect(first.state.triage.connection == .online)
        #expect(second.state.triage.connection == .online)

        await first.send(.triage(.disappeared))
        await first.finish() // Finish before closing the producer: cancellation must own this.
        #expect(terminations.wasCancelled("a") == true)
        #expect(terminations.wasCancelled("b") == nil, "The second subscription must still be open")
        b.continuation.yield(TriageInboxUpdate(identity: identity, overview: overview, connection: .offline("second scene")))
        await second.receive(\.triage.inboxUpdated)
        #expect(second.state.triage.connection == .offline("second scene"))
        #expect(second.state.triage.hasSnapshot)
        await second.send(.triage(.disappeared))
        await second.finish()
        #expect(terminations.wasCancelled("b") == true)
        a.continuation.finish()
        b.continuation.finish()
    }
}

// AsyncStream calls onTermination from the cancelling task. A synchronous mutex
// records completion without spawning an unstructured task or waiting on a timer.
private final class StreamTerminations: Sendable {
    private let cancellations = Mutex<[String: Bool]>([:])

    func record(_ scene: String, reason: AsyncStream<TriageInboxUpdate>.Continuation.Termination) {
        let cancelled: Bool
        switch reason {
        case .cancelled: cancelled = true
        case .finished: cancelled = false
        @unknown default: cancelled = false
        }
        cancellations.withLock { $0[scene] = cancelled }
    }

    func wasCancelled(_ scene: String) -> Bool? {
        cancellations.withLock { $0[scene] }
    }
}
