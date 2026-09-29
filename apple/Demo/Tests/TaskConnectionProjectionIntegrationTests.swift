import AlondraDomain
import AlondraShared
import AlondraWorkspaceFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct TaskConnectionProjectionIntegrationTests {
    @Test(.timeLimit(.minutes(1))) func sceneObservationKeepsTasksConnectionCurrentAfterLeavingInbox() async throws {
        let fixture = V2Fixture()
        let identity = V2Fixture.identity
        let other = V2Fixture.other
        let updates = AsyncStream<DomainWorkspaceUpdate>.makeStream()
        let subscribed = AsyncStream<Void>.makeStream()
        let received = AsyncStream<Void>.makeStream()
        let subscriptions = LockIsolated(0)
        let terminations = LockIsolated(0)
        updates.continuation.onTermination = { _ in terminations.withValue { $0 += 1 } }
        let log = CommandLog()
        let store = Store(initialState: ServiceRootFeature.State()) {
            CombineReducers {
                ServiceRootFeature()
                Reduce<ServiceRootFeature.State, ServiceRootFeature.Action> { _, action in
                    if case .triage(.triage(.workspaceUpdated)) = action { received.continuation.yield(()) }
                    return .none
                }
            }
        } withDependencies: {
            $0.continuousClock = ImmediateClock()
            $0.deliveryJournal = .init(available: false, refresh: { throw CancellationError() }, resolve: { _, _, _, _, _ in throw CancellationError() })
            $0.domainWorkspace = .scripted(log, observe: {
                subscriptions.withValue { $0 += 1 }
                subscribed.continuation.yield(())
                return updates.stream
            }) { _ in .obsolete }
        }
        let observation = store.send(.appeared)
        var started = subscribed.stream.makeAsyncIterator()
        _ = await started.next()
        var delivered = received.stream.makeAsyncIterator()
        updates.continuation.yield(fixture.update(identity))
        _ = await delivered.next()
        #expect(store.tasks.connection == .online)
        #expect(store.tasks.board.hasSnapshot)
        await store.send(.tabSelected(.tasks)).finish()
        await store.send(.triage(.presentationDisappeared)).finish()
        #expect(subscriptions.value == 1 && terminations.value == 0)
        // A disconnected sync may retain its last projection. The exact reason still reaches
        // Tasks; hasSnapshot must not be used as a connectivity test.
        updates.continuation.yield(fixture.update(identity, connection: .offline(.macNotAnswering)))
        _ = await delivered.next()
        #expect(store.tasks.connection == .offline(.macNotAnswering))
        #expect(store.tasks.board.hasSnapshot)
        updates.continuation.yield(DomainWorkspaceUpdate(identity: identity, workspace: nil, connection: .offline(.macNotAnswering)))
        _ = await delivered.next()
        #expect(!store.tasks.board.hasSnapshot)
        #expect(store.tasks.connection == .offline(.macNotAnswering))
        updates.continuation.yield(fixture.update(other))
        _ = await delivered.next()
        #expect(store.tasks.connection == .offline(.macNotAnswering), "another session's update is ignored")
        updates.continuation.yield(fixture.update(identity))
        _ = await delivered.next()
        #expect(store.tasks.connection == .online)
        #expect(store.tasks.board.hasSnapshot)
        await store.send(.triage(.triage(.identityChanged(other)))).finish()
        #expect(store.tasks.connection == .connecting)
        #expect(subscriptions.value == 1)
        await store.send(.disappeared).finish()
        await observation.finish()
        #expect(terminations.value == 1)
        subscribed.continuation.finish()
        received.continuation.finish()
    }
}
