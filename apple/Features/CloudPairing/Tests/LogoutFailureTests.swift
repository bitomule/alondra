import AlondraCloudPairingFeature
import ComposableArchitecture
import Testing
import Foundation

@MainActor
struct LogoutFailureTests {
    @Test func failedLogoutKeepsRecoveryStateAndRetryCompletesBeforeSignedOut() async {
        var initial = CloudPairingFeature.State()
        initial.configuration = .saved
        initial.phase = .approved
        let logout = ControlledLogout()
        let store = TestStore(initialState: initial) { CloudPairingFeature() } withDependencies: {
            $0.cloudPairing = CloudPairingService(
                restore: { Issue.record("Must not restore during incomplete logout"); return false },
                login: { Issue.record("Must not authorize during incomplete logout") },
                begin: { _ in "" }, approve: {}, cancel: {}, logout: { try await logout.run() })
        }
        await store.send(.logout) {
            $0.generation = 1
            $0.phase = .signingOut
        }
        await store.receive(\.failed) {
            $0.phase = .restoring
            $0.logoutIncomplete = true
            $0.error = "Revocation unavailable"
        }
        await store.send(.login)
        await store.send(.reauthenticate)
        await store.send(.retryRestore)
        await store.receive(\.logout) {
            $0.generation = 2
            $0.phase = .signingOut
            $0.error = nil
        }
        await store.receive(\.loggedOut) {
            $0.configuration = .missing
            $0.phase = .signedOut
            $0.logoutIncomplete = false
        }
        await store.finish()
    }

    @Test func pendingRevocationOnRestoreOffersOnlyLogoutRetry() async {
        let store = TestStore(initialState: CloudPairingFeature.State()) { CloudPairingFeature() } withDependencies: {
            $0.cloudPairing = CloudPairingService(
                restore: { throw CloudRestoreError.revocationRequired("Incomplete logout") },
                login: { Issue.record("Must not authorize") }, begin: { _ in "" }, approve: {}, cancel: {}, logout: {})
            $0.cloudPairing.hasSavedConfiguration = { true }
        }
        await store.send(.appeared) { $0.restorationInFlight = true }
        await store.receive(\.configurationLoaded) { $0.configuration = .saved }
        await store.receive(\.revocationRequired) {
            $0.restorationInFlight = false
            $0.logoutIncomplete = true
            $0.error = "Incomplete logout"
        }
        await store.send(.reauthenticate)
        await store.finish()
    }
}

private actor ControlledLogout {
    private var failed = false
    func run() throws {
        if !failed { failed = true; throw Failure() }
    }
    private struct Failure: LocalizedError {
        var errorDescription: String? { "Revocation unavailable" }
    }
}
