import AlondraMacFeature
import AlondraSync
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct MacWindowFeatureTests {
    @Test
    func sharedStreamKeepsWindowSelectionIndependentAndCancelsWhenClosed() async throws {
        let (events, continuation) = AsyncStream<SyncState>.makeStream()
        let identity = SyncIdentity(
            server: URL(string: "http://127.0.0.1:47800")!,
            deviceID: "mac-a",
            credentialRevision: "key-a"
        )
        let state = SyncState(identity: identity, generation: 0, connection: .online, snapshot: nil)
        let store = TestStore(initialState: MacWindowFeature.State()) {
            MacWindowFeature()
        } withDependencies: {
            $0.macWindowSync = MacWindowSync(subscribe: { events })
            $0.macWindowProjection = MacWindowProjection(project: { _ in
                MacWindowUpdate(
                    identity: MacSyncIdentity(server: identity.server, deviceID: identity.deviceID, credentialRevision: identity.credentialRevision),
                    generation: 0,
                    connection: .online,
                    hasSnapshot: true,
                    inboxIDs: ["q1", "q2"]
                )
            })
        }

        await store.send(.appeared)
        continuation.yield(state)
        await store.receive(\.received) {
            $0.identity = MacSyncIdentity(server: identity.server, deviceID: identity.deviceID, credentialRevision: identity.credentialRevision)
            $0.connection = .online
            $0.hasSnapshot = true
            $0.inboxIDs = ["q1", "q2"]
            $0.selection = .item("q1")
            $0.didAutoSelect = true
        }
        await store.send(\.binding.selection, .item("q2")) {
            $0.selection = .item("q2")
        }
        await store.send(.localInboxChanged(["q1"])) {
            $0.inboxIDs = ["q1"]
            $0.selection = .item("q1")
        }
        await store.send(.disappeared)
        continuation.finish()
        await store.finish()
    }

    @Test
    func changingIdentityClearsOldCardAndDoesNotShowItOffline() async {
        let server = URL(string: "http://127.0.0.1:47800")!
        let first = MacSyncIdentity(server: server, deviceID: "mac-a", credentialRevision: "key-a")
        let second = MacSyncIdentity(server: server, deviceID: "mac-b", credentialRevision: "key-b")
        let store = TestStore(initialState: MacWindowFeature.State()) { MacWindowFeature() }

        await store.send(.received(MacWindowUpdate(identity: first, generation: 0, connection: .online, hasSnapshot: true, inboxIDs: ["q1"]))) {
            $0.identity = first
            $0.connection = .online
            $0.hasSnapshot = true
            $0.inboxIDs = ["q1"]
            $0.selection = .item("q1")
            $0.didAutoSelect = true
        }
        await store.send(.showNewRequest) {
            $0.isNewRequestPresented = true
            $0.newRequestOrigin = first
            $0.newRequestOriginEpoch = 0
        }
        #expect(MacWindowFeature.canSubmitNewRequest(store.state, origin: first, epoch: 0))
        await store.send(\.binding.query, "old-server-search") {
            $0.query = "old-server-search"
        }
        await store.send(.identityWillChange(second)) {
            $0.presentationEpoch = 1
            $0.pendingIdentity = second
            $0.requiredGeneration = 1
            $0.identity = nil
            $0.connection = .connecting
            $0.hasSnapshot = false
            $0.inboxIDs = []
            $0.selection = nil
            $0.didAutoSelect = false
            $0.query = ""
            $0.isNewRequestPresented = false
            $0.newRequestOrigin = nil
            $0.newRequestOriginEpoch = nil
        }
        #expect(!MacWindowFeature.canSubmitNewRequest(store.state, origin: first, epoch: 0))
        // SwiftUI may deliver a queued inbox callback from A after the switch began.
        await store.send(.localInboxChanged(["q1"]))
        // This update was queued before the switch. It must not restore A or its sheet.
        await store.send(.received(MacWindowUpdate(identity: first, generation: 0, connection: .online, hasSnapshot: true, inboxIDs: ["q1"])))
        await store.send(.received(MacWindowUpdate(identity: second, generation: 1, connection: .offline("unreachable"), hasSnapshot: false, inboxIDs: nil))) {
            $0.pendingIdentity = nil
            $0.identity = second
            $0.generation = 1
            $0.connection = .offline("unreachable")
            $0.hasSnapshot = false
            $0.inboxIDs = []
            $0.selection = nil
            $0.didAutoSelect = false
        }
        #expect(!MacWindowFeature.canSubmitNewRequest(store.state, origin: first, epoch: 0))
    }

    @Test
    func slowDecodedResponseFromOldIdentityCannotReachWindow() {
        let server = URL(string: "http://127.0.0.1:47800")!
        let old = MacSyncIdentity(server: server, deviceID: "mac-a", credentialRevision: "key-a")
        let new = MacSyncIdentity(server: server, deviceID: "mac-a", credentialRevision: "key-b")
        var gate = MacProjectionGate(expected: old)

        // A began decoding before a key rotation. The window switches first; A completes later.
        gate.use(new)
        let staleBefore = gate.accept(old, generation: 0)
        let current = gate.accept(new, generation: 1)
        let staleAfter = gate.accept(old, generation: 0)
        let oldGeneration = gate.accept(new, generation: 0)
        #expect(!staleBefore)
        #expect(current)
        #expect(!staleAfter)
        #expect(!oldGeneration)
    }

    @Test
    func closingOneWindowDoesNotCancelAnotherWindowsSubscription() async {
        let (firstEvents, firstContinuation) = AsyncStream<SyncState>.makeStream()
        let (secondEvents, secondContinuation) = AsyncStream<SyncState>.makeStream()
        let identity = SyncIdentity(
            server: URL(string: "http://127.0.0.1:47800")!,
            deviceID: "mac-a",
            credentialRevision: "key-a"
        )
        let projectedIdentity = MacSyncIdentity(
            server: identity.server,
            deviceID: identity.deviceID,
            credentialRevision: identity.credentialRevision
        )
        let first = TestStore(initialState: MacWindowFeature.State(windowID: UUID())) {
            MacWindowFeature()
        } withDependencies: {
            $0.macWindowSync = MacWindowSync(subscribe: { firstEvents })
            $0.macWindowProjection = MacWindowProjection(project: { _ in
                MacWindowUpdate(identity: projectedIdentity, generation: 0, connection: .offline("first"), hasSnapshot: false, inboxIDs: nil)
            })
        }
        let second = TestStore(initialState: MacWindowFeature.State(windowID: UUID())) {
            MacWindowFeature()
        } withDependencies: {
            $0.macWindowSync = MacWindowSync(subscribe: { secondEvents })
            $0.macWindowProjection = MacWindowProjection(project: { _ in
                MacWindowUpdate(identity: projectedIdentity, generation: 0, connection: .offline("second"), hasSnapshot: false, inboxIDs: nil)
            })
        }

        await first.send(.appeared)
        await second.send(.appeared)
        firstContinuation.yield(SyncState(identity: identity, generation: 0, connection: .offline("first"), snapshot: nil))
        await first.receive(\.received) {
            $0.identity = projectedIdentity
            $0.connection = .offline("first")
        }
        await first.send(.disappeared)
        firstContinuation.finish()
        await first.finish()

        secondContinuation.yield(SyncState(identity: identity, generation: 0, connection: .offline("second"), snapshot: nil))
        await second.receive(\.received) {
            $0.identity = projectedIdentity
            $0.connection = .offline("second")
        }
        await second.send(.disappeared)
        secondContinuation.finish()
        await second.finish()
    }
}
