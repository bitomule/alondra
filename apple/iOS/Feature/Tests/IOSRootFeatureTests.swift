import AlondraiOSFeature
import AlondraSync
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct IOSRootFeatureTests {
    @Test
    func sceneOwnsSubscriptionAndRetainsItsTabAcrossReconnect() async {
        let (events, continuation) = AsyncStream<SyncState>.makeStream()
        let identity = SyncIdentity(server: URL(string: "https://example.test")!, deviceID: "phone", credentialRevision: "key-1")
        let transportState = SyncState(identity: identity, generation: 0, connection: .online, snapshot: nil)
        let update = IOSRootUpdate(
            identity: IOSSyncIdentity(server: identity.server, deviceID: identity.deviceID, credentialRevision: identity.credentialRevision),
            generation: 0, connection: .online, hasSnapshot: true, inboxCount: 3
        )
        let store = TestStore(initialState: IOSRootFeature.State()) { IOSRootFeature() } withDependencies: {
            $0.iosRootSync = IOSRootSync(subscribe: { events })
            $0.iosRootProjection = IOSRootProjection(project: { _ in update })
        }

        await store.send(.sceneActive(true)) { $0.isActive = true }
        continuation.yield(transportState)
        await store.receive(\.received) {
            $0.identity = update.identity
            $0.connection = .online
            $0.hasSnapshot = true
            $0.inboxCount = 3
            $0.presentationGeneration = 1
        }
        await store.send(\.binding.tab, .tasks) { $0.tab = .tasks }
        await store.send(.inboxCountChanged(2)) { $0.inboxCount = 2 }
        await store.send(.received(IOSRootUpdate(
            identity: update.identity, generation: 0, connection: .offline("temporary"),
            hasSnapshot: false, inboxCount: nil
        ))) {
            $0.connection = .offline("temporary")
            $0.hasSnapshot = false
        }
        await store.send(.sceneActive(false)) { $0.isActive = false }
        continuation.finish()
        await store.finish()
    }

    @Test
    func identityChangeRemovesOldBadgeAndNavigationEvenWhileOffline() async {
        let a = IOSSyncIdentity(server: URL(string: "https://a.example")!, deviceID: "phone-a", credentialRevision: "key-a")
        let b = IOSSyncIdentity(server: URL(string: "https://b.example")!, deviceID: "phone-b", credentialRevision: "key-b")
        let store = TestStore(initialState: IOSRootFeature.State()) { IOSRootFeature() }
        await store.send(.sceneActive(true)) { $0.isActive = true }
        await store.send(.received(IOSRootUpdate(identity: a, generation: 0, connection: .online, hasSnapshot: true, inboxCount: 5))) {
            $0.identity = a
            $0.connection = .online
            $0.hasSnapshot = true
            $0.inboxCount = 5
            $0.presentationGeneration = 1
        }
        await store.send(\.binding.tab, .tasks) { $0.tab = .tasks }
        await store.send(.received(IOSRootUpdate(identity: b, generation: 1, connection: .offline("unreachable"), hasSnapshot: false, inboxCount: nil))) {
            $0.identity = b
            $0.generation = 1
            $0.connection = .offline("unreachable")
            $0.hasSnapshot = false
            $0.inboxCount = 0
            $0.tab = .triage
            $0.presentationGeneration = 2
        }
        await store.send(.sceneActive(false)) { $0.isActive = false }
        await store.finish()
    }

    @Test
    func invalidConfigurationClearsPresentationEvenWhenServerIdentityIsUnchanged() async {
        let identity = IOSSyncIdentity(server: URL(string: "https://a.example")!, deviceID: "phone", credentialRevision: "key")
        let store = TestStore(initialState: IOSRootFeature.State()) { IOSRootFeature() }
        await store.send(.sceneActive(true)) { $0.isActive = true }
        await store.send(.received(IOSRootUpdate(identity: identity, generation: 0, connection: .online, hasSnapshot: true, inboxCount: 4))) {
            $0.identity = identity
            $0.connection = .online
            $0.hasSnapshot = true
            $0.inboxCount = 4
            $0.presentationGeneration = 1
        }
        await store.send(\.binding.tab, .tasks) { $0.tab = .tasks }
        await store.send(.received(IOSRootUpdate(
            identity: identity, generation: 1, connection: .offline("Invalid server URL"),
            hasSnapshot: false, inboxCount: 0, resetsPresentation: true
        ))) {
            $0.generation = 1
            $0.connection = .offline("Invalid server URL")
            $0.hasSnapshot = false
            $0.inboxCount = 0
            $0.tab = .triage
            $0.presentationGeneration = 2
        }
        await store.send(.sceneActive(false)) { $0.isActive = false }
        await store.finish()
    }

    @Test
    func stoppingOneSceneDoesNotCancelAnotherScenesSubscription() async {
        let (eventsA, continuationA) = AsyncStream<SyncState>.makeStream()
        let (eventsB, continuationB) = AsyncStream<SyncState>.makeStream()
        let identity = SyncIdentity(server: URL(string: "https://a.example")!, deviceID: "phone", credentialRevision: "key")
        let transportState = SyncState(identity: identity, generation: 0, connection: .online, snapshot: nil)
        let update = IOSRootUpdate(
            identity: IOSSyncIdentity(server: identity.server, deviceID: identity.deviceID, credentialRevision: identity.credentialRevision),
            generation: 0, connection: .online, hasSnapshot: false, inboxCount: nil
        )
        let first = TestStore(initialState: IOSRootFeature.State()) { IOSRootFeature() } withDependencies: {
            $0.iosRootSync = IOSRootSync(subscribe: { eventsA })
            $0.iosRootProjection = IOSRootProjection(project: { _ in update })
        }
        let second = TestStore(initialState: IOSRootFeature.State()) { IOSRootFeature() } withDependencies: {
            $0.iosRootSync = IOSRootSync(subscribe: { eventsB })
            $0.iosRootProjection = IOSRootProjection(project: { _ in update })
        }
        await first.send(.sceneActive(true)) { $0.isActive = true }
        await second.send(.sceneActive(true)) { $0.isActive = true }
        await first.send(.sceneActive(false)) { $0.isActive = false }
        continuationB.yield(transportState)
        await second.receive(\.received) {
            $0.identity = update.identity
            $0.connection = .online
            $0.presentationGeneration = 1
        }
        await second.send(.sceneActive(false)) { $0.isActive = false }
        continuationA.finish()
        continuationB.finish()
        await first.finish()
        await second.finish()
    }
}
