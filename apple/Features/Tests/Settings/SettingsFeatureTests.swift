import AlondraFeatures
import ComposableArchitecture
import Testing
import Foundation

@MainActor struct SettingsFeatureTests {
    @Test func anotherWindowCannotSilentlyOverwriteAnUnsavedQuietHoursDraft() async {
        let initial = QuietHours(enabled: true, start: 1380, end: 480)
        let incoming = QuietHours(enabled: true, start: 1320, end: 420)
        var state = SettingsFeature.State(service: "service", device: "client-1", supportsNotifications: true)
        state.notifications = NotificationSettings(permission: .authorized, delivery: .registered, quietHours: initial)
        state.baseline = initial; state.draft = initial
        let store = TestStore(initialState: state) { SettingsFeature() } withDependencies: {
            $0.serviceSettings.save = { _, _ in Issue.record("A conflicted draft was persisted") }
        }
        await store.send(.quietStart(1260)) { $0.draft.start = 1260; $0.editing = true }
        let update = NotificationSettings(permission: .authorized, delivery: .registering, quietHours: incoming)
        await store.send(.updated(update)) { $0.notifications = update; $0.conflict = true }
        await store.send(.save)
        #expect(store.state.draft.start == 1260)
        #expect(!store.state.canSave)
        await store.send(.useCurrent) {
            $0.baseline = incoming; $0.draft = incoming; $0.editing = false; $0.conflict = false
        }
    }

    @Test func completionFromClosedSettingsCannotReplaceFreshConnectionState() async {
        var state = SettingsFeature.State(service: "service", device: "client-1", supportsNotifications: false)
        state.checking = true
        let store = TestStore(initialState: state) { SettingsFeature() }
        await store.send(.disappeared) { $0.generation = 1; $0.checking = false }
        await store.send(.checked(0))
        await store.send(.failed("old failure", 0))
        #expect(!store.state.connected)
        #expect(store.state.error == nil)
    }
}

@MainActor struct AlertsNeedAccountTests {
    private func mac(_ n: Int, local: Bool) -> MacChoice {
        MacChoice(id: UUID(uuidString: "00000000-0000-0000-0000-00000000000\(n)")!, macID: "mac-\(n)", local: local)
    }

    @Test func onlyMacsLinkedWithoutAnAccountSayAlertsNeedOne() async {
        var state = SettingsFeature.State(service: "service", device: "phone", supportsNotifications: false, usesAccount: false)
        state.macs = [mac(1, local: true), mac(2, local: true)]
        #expect(state.alertsNeedAccount)
        let store = TestStore(initialState: state) { SettingsFeature() } withDependencies: {
            $0.serviceSettings.observe = { Issue.record("Alerts were set up for Macs without an account"); return AsyncStream { $0.finish() } }
        }
        await store.send(.appeared)
    }

    @Test func aMacWithAnAccountOrNoMacAtAllSaysNothing() {
        var state = SettingsFeature.State(service: "service", device: "phone", supportsNotifications: true)
        state.macs = [mac(1, local: true), mac(2, local: false)]
        #expect(!state.alertsNeedAccount)
        state.macs = []
        #expect(!state.alertsNeedAccount)
    }
}

@MainActor struct MacSelectionSettingsTests {
    @Test func selectionFailurePreservesActiveMacAndUnknownChoiceDoesNothing() async {
        let a = UUID(), b = UUID()
        var state = SettingsFeature.State(service: "service", device: "phone", supportsNotifications: false)
        state.macs = [.init(id: a, macID: "mac-a"), .init(id: b, macID: "mac-b")]
        state.selectedMacID = a
        let store = TestStore(initialState: state) { SettingsFeature() } withDependencies: {
            $0.serviceSettings.selectMac = { id in
                #expect(id == b)
                throw Failure.offline
            }
            $0.serviceSettings.problem = { _ in .macNotAnswering }
        }
        await store.send(.selectMac(UUID()))
        await store.send(.selectMac(a))
        await store.send(.selectMac(b)) { $0.selectingMacID = b }
        await store.receive(\.macSelectionFailed) { $0.selectingMacID = nil; $0.macError = .macNotAnswering }
        #expect(store.state.selectedMacID == a)
    }

    @Test func cancelledMacSelectionShowsNoError() async {
        let a = UUID(), b = UUID()
        var state = SettingsFeature.State(service: "service", device: "phone", supportsNotifications: false)
        state.macs = [.init(id: a, macID: "mac-a"), .init(id: b, macID: "mac-b")]
        state.selectedMacID = a
        let store = TestStore(initialState: state) { SettingsFeature() } withDependencies: {
            $0.serviceSettings.selectMac = { _ in throw CancellationError() }
            $0.serviceSettings.problem = { _ in Issue.record("Cancellation is not a problem"); return .unexpected }
        }
        await store.send(.selectMac(b)) { $0.selectingMacID = b }
        await store.receive(\.macSelectionFailed) { $0.selectingMacID = nil }
        #expect(store.state.macError == nil)
    }
    @Test func completionAfterClosingCannotSelectAnotherMac() async {
        let a = UUID(), b = UUID()
        var state = SettingsFeature.State(service: "service", device: "phone", supportsNotifications: false)
        state.macs = [.init(id: a, macID: "mac-a"), .init(id: b, macID: "mac-b")]
        state.selectedMacID = a; state.selectingMacID = b
        let store = TestStore(initialState: state) { SettingsFeature() }
        await store.send(.disappeared) { $0.generation = 1; $0.selectingMacID = nil }
        await store.send(.macSelected(b, 0))
        await store.send(.macSelectionFailed(.macNotAnswering, 0))
        #expect(store.state.selectedMacID == a)
        #expect(store.state.macError == nil)
    }
    enum Failure: LocalizedError { case offline; var errorDescription: String? { "The Mac is offline." } }
}

@MainActor struct AdditionalMacSettingsTests {
    @Test func linkingAndDismissingAnotherMacNeverLogsOutOrRestoresExistingAccount() async {
        let gate = PairingCleanupGate()
        let store = TestStore(initialState: SettingsFeature.State(service: "service", device: "phone", supportsNotifications: false)) {
            SettingsFeature()
        } withDependencies: {
            $0.cloudPairing.cancel = { await gate.cancel() }
            $0.cloudPairing.logout = { Issue.record("Linking another Mac logged out") }
            $0.cloudPairing.restore = { Issue.record("Linking another Mac reran onboarding restore"); return false }
        }
        await store.send(.linkMac) { $0.pairing = .init(additionalMac: true) }
        await store.send(.pairing(.presented(.appeared)))
        await store.send(.pairing(.presented(.cancel))) { $0.pairing?.generation = 1; $0.pairing?.phase = .cancelling }
        await store.receive(\.pairing.presented.cancelled) { $0.pairing?.phase = .scanning }
        await store.send(.pairing(.dismiss)) { $0.pairing = nil; $0.pairingCleanup = true }
        await gate.waitForCleanup()
        await store.send(.linkMac)
        await gate.resume()
        await store.receive(\.pairingCleanupFinished) { $0.pairingCleanup = false }
        await store.finish()
    }

    @Test func useMyAccountSignsInThenAsksForTheMacsCodeWithoutDroppingItsLocalMacs() async {
        var state = SettingsFeature.State(service: "service", device: "phone", supportsNotifications: false, usesAccount: false)
        state.macs = [MacChoice(id: UUID(), macID: "mac-1", local: true)]
        let store = TestStore(initialState: state) { SettingsFeature() } withDependencies: {
            $0.cloudPairing.login = {}
            $0.cloudPairing.logout = { Issue.record("Signing in from Settings logged out") }
        }
        await store.send(.useAccount) { $0.pairing = .init(linksAccount: true) }
        #expect(store.state.pairing?.phase == .signedOut)
        #expect(store.state.pairing?.showsAccount == true)
        await store.send(.pairing(.presented(.login))) { $0.pairing?.phase = .loggingIn }
        await store.receive(\.pairing.presented.loggedIn) { $0.pairing?.signedIn = true; $0.pairing?.phase = .scanning }
        await store.finish()
    }

    @Test func useMyAccountIsOnlyOfferedWhileEveryMacIsLinkedWithoutOne() async {
        var state = SettingsFeature.State(service: "service", device: "phone", supportsNotifications: true)
        state.macs = [MacChoice(id: UUID(), macID: "mac-1", local: false)]
        let store = TestStore(initialState: state) { SettingsFeature() }
        await store.send(.useAccount)
    }
}

private actor PairingCleanupGate {
    private var count = 0
    private var continuation: CheckedContinuation<Void, Never>?
    private var observer: CheckedContinuation<Void, Never>?
    func cancel() async {
        count += 1
        if count == 2 {
            await withCheckedContinuation {
                continuation = $0
                observer?.resume(); observer = nil
            }
        }
    }
    func waitForCleanup() async {
        if continuation == nil { await withCheckedContinuation { observer = $0 } }
    }
    func resume() { continuation?.resume(); continuation = nil }
}
