import AlondraFeatures
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct SignOutWithLocalMacsTests {
    @Test func signingOutWithMacsPairedOnTheNetworkStaysInTheWorkspace() async {
        let restoredAfterSignOut = LockIsolated(false)
        let signedOut = LockIsolated(false)
        var client = SignInClient(restore: { Issue.record("Restoring is not part of signing out"); return false },
                                  login: {}, begin: { _ in "" }, approve: {}, cancel: {}, logout: { signedOut.setValue(true) })
        client.keepsLocalMacs = {
            restoredAfterSignOut.setValue(signedOut.value)
            return true
        }
        var state = SignInFeature.State()
        state.phase = .approved
        state.configuration = .saved
        let store = TestStore(initialState: state) { SignInFeature() } withDependencies: {
            $0.cloudPairing = client
        }
        await store.send(.logout) { $0.generation = 1; $0.phase = .signingOut }
        await store.receive(\.restored) { $0.phase = .approved; $0.configuration = .saved }
        #expect(restoredAfterSignOut.value)
    }

    @Test func signingOutWithNothingLeftShowsTheFirstScreen() async {
        var state = SignInFeature.State()
        state.phase = .approved
        state.configuration = .saved
        let store = TestStore(initialState: state) { SignInFeature() } withDependencies: {
            $0.cloudPairing = SignInClient(restore: { false }, login: {}, begin: { _ in "" }, approve: {}, cancel: {}, logout: {})
        }
        await store.send(.logout) { $0.generation = 1; $0.phase = .signingOut }
        await store.receive(\.loggedOut) { $0.phase = .signedOut; $0.configuration = .missing }
    }
}

@MainActor
struct LocalNetworkPermissionTests {
    private func client(permission: LocalNetworkPermission, updates: [FindMacUpdate] = [],
                        opened: LockIsolated<Bool> = LockIsolated(false), browsed: LockIsolated<Int> = LockIsolated(0)) -> FindMacClient {
        FindMacClient(permission: { permission }, browse: {
            browsed.withValue { $0 += 1 }
            return AsyncStream { continuation in
                for update in updates { continuation.yield(update) }
                continuation.finish()
            }
        }, pair: { _ in throw CancellationError() }, approve: {}, cancel: {}, describe: { _ in "" },
        openSettings: { opened.setValue(true) })
    }

    @Test func theSystemIsAskedOnlyAfterTheExplanation() async {
        let browsed = LockIsolated(0)
        let store = TestStore(initialState: FindMacFeature.State()) { FindMacFeature() } withDependencies: {
            $0.findMac = client(permission: .unknown, browsed: browsed)
        }
        await store.send(.appeared) { $0.explainsPermission = true }
        #expect(browsed.value == 0)
        await store.send(.allowSearch) { $0.explainsPermission = false }
        await store.finish()
        #expect(browsed.value == 1)
    }

    @Test func aDeniedPermissionShowsTheWayToSettingsInsteadOfSearchingForever() async {
        let opened = LockIsolated(false)
        let store = TestStore(initialState: FindMacFeature.State()) { FindMacFeature() } withDependencies: {
            $0.findMac = client(permission: .allowed, updates: [.denied], opened: opened)
        }
        await store.send(.appeared)
        await store.receive(\.permissionWasDenied) { $0.permissionDenied = true }
        await store.send(.openSettings)
        await store.finish()
        #expect(opened.value)
    }

    @Test func turningItBackOnBringsTheListBack() async {
        let studio = FoundMac(id: "Studio", name: "Studio", paired: false)
        let store = TestStore(initialState: FindMacFeature.State()) { FindMacFeature() } withDependencies: {
            $0.findMac = client(permission: .denied, updates: [.macs([studio])])
        }
        await store.send(.appeared) { $0.permissionDenied = true }
        await store.receive(\.macsChanged) { $0.macs = [studio]; $0.permissionDenied = false }
        await store.finish()
    }

    @Test func allowingAccessLeavesTheRefusalEvenWithNoMacsAround() async {
        let store = TestStore(initialState: FindMacFeature.State()) { FindMacFeature() } withDependencies: {
            $0.findMac = client(permission: .denied, updates: [.allowed])
        }
        await store.send(.appeared) { $0.permissionDenied = true }
        await store.receive(\.permissionWasAllowed) { $0.permissionDenied = false }
        await store.finish()
    }
}
