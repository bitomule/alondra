import AlondraFeatures
import AlondraContract
import ComposableArchitecture
import Foundation
import Testing

@MainActor struct WorkspaceRestorationTests {
    @Test func savedWorkspaceStaysHomeDuringSlowRestoreAndOfflineFailure() async {
        let (network, continuation) = AsyncStream<Void>.makeStream()
        let clock = TestClock()
        let store = TestStore(initialState: SignInFeature.State()) { SignInFeature() } withDependencies: {
            $0.continuousClock = clock
            $0.withRandomNumberGenerator = .init(LowestRandom())
            $0.cloudPairing.hasSavedConfiguration = { true }
            $0.cloudPairing.restore = {
                for await _ in network { break }
                throw URLError(.notConnectedToInternet)
            }
            $0.cloudPairing.classify = { _ in .noInternet }
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
            $0.restoreProblem = .noInternet
            $0.restoreFailures = 1
        }
        #expect(store.state.showsWorkspace)
        #expect(store.state.configuration == .saved)
        #expect(!store.state.reauthenticationRequired)
        await store.skipInFlightEffects()
    }

    @Test func failedRestoreIsRetriedAutomaticallyWithBackoffAndWinsWhenTheMacAnswers() async {
        let clock = TestClock()
        let attempts = LockIsolated(0)
        let store = TestStore(initialState: SignInFeature.State()) { SignInFeature() } withDependencies: {
            $0.continuousClock = clock
            $0.withRandomNumberGenerator = .init(LowestRandom())
            $0.cloudPairing.hasSavedConfiguration = { true }
            $0.cloudPairing.restore = {
                attempts.withValue { $0 += 1 }
                if attempts.value < 3 { throw URLError(.timedOut) }
                return true
            }
            $0.cloudPairing.classify = { _ in .serverUnreachable }
            $0.cloudPairing.logout = { Issue.record("A network failure must never remove the pairing") }
        }
        store.exhaustivity = .off
        await store.send(.appeared)
        await store.receive(\.restorationFailed)
        #expect(store.state.restoreProblem == .serverUnreachable)
        await clock.advance(by: .milliseconds(500))
        await store.receive(\.retryRestore)
        await store.receive(\.restorationFailed)
        #expect(store.state.restoreFailures == 2)
        #expect(attempts.value == 2)
        await clock.advance(by: .milliseconds(999))
        #expect(attempts.value == 2, "The second wait backs off to at least one second")
        await clock.advance(by: .seconds(2))
        await store.receive(\.retryRestore)
        await store.receive(\.restored)
        #expect(store.state.phase == .approved)
        #expect(store.state.restoreProblem == nil)
        #expect(store.state.restoreFailures == 0)
        await store.finish()
    }

    @Test func aRefusedAccessCheckKeepsTheSessionAndRetriesUntilItAnswers() async {
        let clock = TestClock()
        let attempts = LockIsolated(0)
        let store = TestStore(initialState: SignInFeature.State()) { SignInFeature() } withDependencies: {
            $0.continuousClock = clock
            $0.withRandomNumberGenerator = .init(LowestRandom())
            $0.cloudPairing.hasSavedConfiguration = { true }
            $0.cloudPairing.restore = {
                attempts.withValue { $0 += 1 }
                if attempts.value == 1 { throw URLError(.userCancelledAuthentication) }
                return true
            }
            $0.cloudPairing.classify = { _ in .accessNotConfirmed }
            $0.cloudPairing.login = { Issue.record("A refused access check must not ask to sign in") }
            $0.cloudPairing.logout = { Issue.record("A refused access check must not sign out") }
        }
        store.exhaustivity = .off
        await store.send(.appeared)
        await store.receive(\.restorationFailed)
        #expect(store.state.restoreProblem == .accessNotConfirmed)
        #expect(!store.state.reauthenticationRequired)
        #expect(store.state.showsWorkspace)
        await clock.advance(by: .milliseconds(500))
        await store.receive(\.retryRestore)
        await store.receive(\.restored)
        #expect(store.state.phase == .approved)
        #expect(store.state.restoreProblem == nil)
        await store.finish()
    }

    @Test func networkReturningSkipsTheWait() async {
        let clock = TestClock()
        let attempts = LockIsolated(0)
        let store = TestStore(initialState: SignInFeature.State()) { SignInFeature() } withDependencies: {
            $0.continuousClock = clock
            $0.withRandomNumberGenerator = .init(LowestRandom())
            $0.cloudPairing.hasSavedConfiguration = { true }
            $0.cloudPairing.restore = {
                attempts.withValue { $0 += 1 }
                if attempts.value == 1 { throw URLError(.notConnectedToInternet) }
                return true
            }
            $0.cloudPairing.classify = { _ in .noInternet }
        }
        store.exhaustivity = .off
        await store.send(.appeared)
        await store.receive(\.restorationFailed)
        await store.send(.retryNow)
        await store.receive(\.retryRestore)
        await store.receive(\.restored)
        #expect(store.state.phase == .approved)
        await store.finish()
    }

    @Test func anExpiredSessionFoundWhileRestoringIsLogged() async {
        let lines = LockIsolated<[String]>([])
        AccountDiagnostics.record(to: { line, _ in lines.withValue { $0.append(line) } })
        defer { AccountDiagnostics.record(to: AccountDiagnostics.systemLog) }
        let store = TestStore(initialState: SignInFeature.State()) { SignInFeature() } withDependencies: {
            $0.continuousClock = TestClock()
            $0.cloudPairing.hasSavedConfiguration = { true }
            $0.cloudPairing.restore = { throw RestoreError.reauthenticationRequired }
        }
        store.exhaustivity = .off
        await store.send(.appeared)
        await store.receive(\.restorationFailed)
        #expect(lines.value.contains("person sees sessionExpired after RestoreError.reauthenticationRequired"))
        await store.finish()
    }

    @Test func expiredSessionWaitsForTheUserAndReauthenticatesWithoutLosingPairing() async {
        let clock = TestClock()
        let store = TestStore(initialState: SignInFeature.State()) { SignInFeature() } withDependencies: {
            $0.continuousClock = clock
            $0.cloudPairing.hasSavedConfiguration = { true }
            $0.cloudPairing.restore = { throw RestoreError.reauthenticationRequired }
            $0.cloudPairing.logout = { Issue.record("Reauthentication must preserve pairing") }
        }
        await store.send(.appeared) { $0.restorationInFlight = true }
        await store.receive(\.configurationLoaded) { $0.configuration = .saved }
        await store.receive(\.restorationFailed) {
            $0.restorationInFlight = false; $0.restoreProblem = .sessionExpired; $0.reauthenticationRequired = true
        }
        await store.send(.retryNow)
        #expect(store.state.showsWorkspace)
        store.dependencies.cloudPairing.login = {}
        store.dependencies.cloudPairing.restore = { true }
        await store.send(.reauthenticate) { $0.restorationInFlight = true }
        await store.receive(\.restored) {
            $0.restorationInFlight = false; $0.reauthenticationRequired = false; $0.phase = .approved
            $0.restoreProblem = nil
        }
        #expect(store.state.configuration == .saved)
        await store.finish()
    }

    @Test func newInstallUsesOnboardingWithoutNetworkAndLogoutReturnsThere() async {
        let store = TestStore(initialState: SignInFeature.State()) { SignInFeature() } withDependencies: {
            $0.cloudPairing.hasSavedConfiguration = { false }
            $0.cloudPairing.restore = { Issue.record("No configured workspace needs no network"); return false }
        }
        await store.send(.appeared) { $0.restorationInFlight = true }
        await store.receive(\.configurationLoaded) { $0.configuration = .missing }
        await store.receive(\.restored) { $0.restorationInFlight = false; $0.phase = .signedOut }
        #expect(!store.state.showsWorkspace)
        await store.send(.approved(0, .authorized)) { $0.phase = .approved; $0.configuration = .saved }
        await store.send(.logout) { $0.generation = 1; $0.phase = .signingOut }
        await store.receive(\.loggedOut) { $0.phase = .signedOut; $0.configuration = .missing }
        await store.send(.restored(0, true))
        #expect(!store.state.showsWorkspace)
        await store.finish()
    }
}

private struct LowestRandom: RandomNumberGenerator {
    mutating func next() -> UInt64 { 0 }
}
