import AlondraDomain
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
        let workspace = V2Fixture().workspace
        let store = Store(initialState: ServiceRootFeature.State()) { ServiceRootFeature() }
        let online = DomainWorkspaceUpdate(identity: identity, workspace: workspace, connection: .online)
        await store.send(.triage(.triage(.workspaceUpdated(online)))).finish()
        await store.send(.tabSelected(.tasks)).finish()
        #expect(store.triage.triage.connection == .online)
        await store.send(.triage(.triage(.workspaceUpdated(DomainWorkspaceUpdate(
            identity: identity, workspace: workspace, connection: .offline(.serverUnreachable)
        ))))).finish()
        #expect(store.tab == .tasks)
        #expect(store.triage.triage.connection == .offline(.serverUnreachable))
        #expect(store.triage.triage.hasSnapshot)
        await store.send(.triage(.triage(.workspaceUpdated(online)))).finish()
        #expect(store.tab == .tasks)
        #expect(store.triage.triage.connection == .online)
        #expect(store.triage.triage.hasSnapshot)
    }

    @Test(.timeLimit(.minutes(1)))
    func closingOneSceneLeavesTheOtherSubscribed() async throws {
        let identity = TriageIdentity(server: URL(string: "https://service.example")!,
            deviceID: "phone", credentialRevision: "approved", generation: 1)
        let workspace = V2Fixture().workspace
        let a = AsyncStream<DomainWorkspaceUpdate>.makeStream()
        let b = AsyncStream<DomainWorkspaceUpdate>.makeStream()
        let terminations = StreamTerminations()
        a.continuation.onTermination = { reason in
            terminations.record("a", reason: reason)
        }
        b.continuation.onTermination = { reason in
            terminations.record("b", reason: reason)
        }
        let first = TestStore(initialState: ServiceTriageFeature.State()) { ServiceTriageFeature() } withDependencies: {
            $0.domainWorkspace = .scripted(CommandLog(), observe: { a.stream }) { _ in .obsolete }
        }
        let second = TestStore(initialState: ServiceTriageFeature.State()) { ServiceTriageFeature() } withDependencies: {
            $0.domainWorkspace = .scripted(CommandLog(), observe: { b.stream }) { _ in .obsolete }
        }
        // Cross-feature integration: child projection effects are consumed without asserting
        // every internal write. The observable connection and snapshot are
        // checked after each stage, and both real subscription effects must finish below.
        first.exhaustivity = .off(showSkippedAssertions: true)
        second.exhaustivity = .off(showSkippedAssertions: true)
        await first.send(.triage(.appeared))
        await second.send(.triage(.appeared))
        let online = DomainWorkspaceUpdate(identity: identity, workspace: workspace, connection: .online)
        a.continuation.yield(online)
        b.continuation.yield(online)
        await first.receive(\.triage.workspaceUpdated)
        await second.receive(\.triage.workspaceUpdated)
        #expect(first.state.triage.connection == .online)
        #expect(second.state.triage.connection == .online)

        await first.send(.triage(.disappeared))
        await first.finish() // Finish before closing the producer: cancellation must own this.
        #expect(terminations.wasCancelled("a") == true)
        #expect(terminations.wasCancelled("b") == nil, "The second subscription must still be open")
        b.continuation.yield(DomainWorkspaceUpdate(identity: identity, workspace: workspace, connection: .offline(.noInternet)))
        await second.receive(\.triage.workspaceUpdated)
        #expect(second.state.triage.connection == .offline(.noInternet))
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

    func record(_ scene: String, reason: AsyncStream<DomainWorkspaceUpdate>.Continuation.Termination) {
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
