import AlondraDomain
import AlondraShared
import AlondraWorkspaceFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct TaskConnectionProjectionIntegrationTests {
    @Test(.timeLimit(.minutes(1))) func sceneObservationKeepsTasksConnectionCurrentAfterLeavingInbox() async throws {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let overview = try decoder.decode(Overview.self, from: Data(#"{"tick":1,"nodes":[],"open_questions":[],"news":[],"orchestrator_messages":[],"machine":{"memory_total":1,"memory_available":1,"load":[0],"disk_available":1,"unreliable":false}}"#.utf8))
        let identity = TriageIdentity(server: URL(string: "https://service.example")!, deviceID: "phone", credentialRevision: "one", generation: 1)
        let other = TriageIdentity(server: identity.server, deviceID: "other", credentialRevision: "two", generation: 2)
        let updates = AsyncStream<TriageInboxUpdate>.makeStream()
        let subscribed = AsyncStream<Void>.makeStream()
        let received = AsyncStream<Void>.makeStream()
        let subscriptions = LockIsolated(0)
        let terminations = LockIsolated(0)
        updates.continuation.onTermination = { _ in terminations.withValue { $0 += 1 } }
        let clock = TestClock()
        let store = Store(initialState: ServiceRootFeature.State()) {
            CombineReducers {
                ServiceRootFeature()
                Reduce<ServiceRootFeature.State, ServiceRootFeature.Action> { _, action in
                    if case .triage(.triage(.inboxUpdated)) = action { received.continuation.yield(()) }
                    return .none
                }
            }
        } withDependencies: {
            $0.continuousClock = clock
            $0.serviceCommandRecovery = .init(observe: { AsyncStream { $0.finish() } })
            $0.deliveryJournal = .init(available: false, refresh: { throw CancellationError() }, resolve: { _, _, _, _, _ in throw CancellationError() })
            $0.triageService = TriageService(observe: {
                subscriptions.withValue { $0 += 1 }
                subscribed.continuation.yield(())
                return updates.stream
            }, mutate: { _, _, _ in .obsolete })
        }
        let observation = store.send(.appeared)
        var started = subscribed.stream.makeAsyncIterator()
        _ = await started.next()
        var delivered = received.stream.makeAsyncIterator()
        updates.continuation.yield(.init(identity: identity, overview: overview, connection: .online))
        _ = await delivered.next()
        await store.send(.tasks(.snapshot(overview))).finish()
        #expect(store.tasks.connection == .online)
        #expect(store.tasks.board.hasSnapshot)
        await store.send(.tabSelected(.tasks)).finish()
        await store.send(.triage(.presentationDisappeared)).finish()
        #expect(subscriptions.value == 1 && terminations.value == 0)
        // A disconnected sync may retain its last task projection. The exact reason
        // still reaches Tasks; hasSnapshot must not be used as a connectivity test.
        updates.continuation.yield(.init(identity: identity, overview: overview, connection: .offline("Connection lost")))
        _ = await delivered.next()
        #expect(store.tasks.connection == .offline("Connection lost"))
        #expect(store.tasks.board.hasSnapshot)
        await store.send(.tasks(.snapshot(nil))).finish()
        #expect(!store.tasks.board.hasSnapshot)
        #expect(store.tasks.connection == .offline("Connection lost"))
        updates.continuation.yield(.init(identity: other, overview: overview, connection: .online))
        _ = await delivered.next()
        #expect(store.tasks.connection == .offline("Connection lost"))
        updates.continuation.yield(.init(identity: identity, overview: overview, connection: .online))
        _ = await delivered.next()
        #expect(store.tasks.connection == .online)
        await store.send(.triage(.triage(.identityChanged(other)))).finish()
        #expect(store.tasks.connection == .connecting)
        #expect(subscriptions.value == 1)
        await store.send(.disappeared).finish()
        await clock.advance(by: .seconds(6))
        await observation.finish()
        #expect(terminations.value == 1)
        subscribed.continuation.finish()
        received.continuation.finish()
    }
}
