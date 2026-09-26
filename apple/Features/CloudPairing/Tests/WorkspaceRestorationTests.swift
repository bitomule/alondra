import AlondraCloudPairingFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor struct WorkspaceRestorationTests {
    @Test func savedWorkspaceStaysHomeDuringSlowRestoreAndOfflineFailure() async {
        let (network, continuation) = AsyncStream<Void>.makeStream()
        let store = TestStore(initialState: CloudPairingFeature.State()) { CloudPairingFeature() } withDependencies: {
            $0.cloudPairing.hasSavedConfiguration = { true }
            $0.cloudPairing.restore = {
                for await _ in network { break }
                throw URLError(.notConnectedToInternet)
            }
            $0.cloudPairing.logout = { Issue.record("Offline restoration must not remove pairing") }
        }
        #expect(store.state.showsWorkspace)
        await store.send(.appeared) { $0.restorationInFlight = true }
        await store.receive(\.configurationLoaded) { $0.configuration = .saved }
        #expect(store.state.showsWorkspace)
        #expect(store.state.phase == .restoring)
        await store.send(.appeared) // Another scene cannot restart the in-flight restore.
        continuation.yield(()); continuation.finish()
        await store.receive(\.restorationFailed) {
            $0.restorationInFlight = false
            $0.error = URLError(.notConnectedToInternet).localizedDescription
        }
        #expect(store.state.showsWorkspace)
        #expect(store.state.configuration == .saved)
        #expect(!store.state.reauthenticationRequired)
        await store.finish()
    }

    @Test func expiredSessionOffersExplicitReauthenticationWithoutLosingPairing() async {
        let store = TestStore(initialState: CloudPairingFeature.State()) { CloudPairingFeature() } withDependencies: {
            $0.cloudPairing.hasSavedConfiguration = { true }
            $0.cloudPairing.restore = { throw CloudRestoreError.reauthenticationRequired("Sign in again") }
            $0.cloudPairing.logout = { Issue.record("Reauthentication must preserve pairing") }
        }
        await store.send(.appeared) { $0.restorationInFlight = true }
        await store.receive(\.configurationLoaded) { $0.configuration = .saved }
        await store.receive(\.restorationFailed) {
            $0.restorationInFlight = false; $0.error = "Sign in again"; $0.reauthenticationRequired = true
        }
        #expect(store.state.showsWorkspace)
        store.dependencies.cloudPairing.login = {}
        store.dependencies.cloudPairing.restore = { true }
        await store.send(.reauthenticate) { $0.restorationInFlight = true; $0.error = nil }
        await store.receive(\.restored) {
            $0.restorationInFlight = false; $0.reauthenticationRequired = false; $0.phase = .approved
        }
        #expect(store.state.configuration == .saved)
        await store.finish()
    }

    @Test func newInstallUsesOnboardingWithoutNetworkAndLogoutReturnsThere() async {
        let store = TestStore(initialState: CloudPairingFeature.State()) { CloudPairingFeature() } withDependencies: {
            $0.cloudPairing.hasSavedConfiguration = { false }
            $0.cloudPairing.restore = { Issue.record("No configured workspace needs no network"); return false }
        }
        await store.send(.appeared) { $0.restorationInFlight = true }
        await store.receive(\.configurationLoaded) { $0.configuration = .missing }
        await store.receive(\.restored) { $0.restorationInFlight = false; $0.phase = .signedOut }
        #expect(!store.state.showsWorkspace)
        await store.send(.approved(0)) { $0.phase = .approved; $0.configuration = .saved }
        await store.send(.logout) { $0.generation = 1; $0.phase = .signingOut }
        await store.receive(\.loggedOut) { $0.phase = .signedOut; $0.configuration = .missing }
        await store.send(.restored(0, true))
        #expect(!store.state.showsWorkspace)
        await store.finish()
    }
}
