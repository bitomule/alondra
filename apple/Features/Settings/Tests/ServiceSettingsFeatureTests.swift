import AlondraSettingsFeature
import ComposableArchitecture
import Testing
import Foundation

@MainActor struct ServiceSettingsFeatureTests {
    @Test func anotherWindowCannotSilentlyOverwriteAnUnsavedQuietHoursDraft() async {
        let initial = ServiceQuietHours(enabled: true, start: 1380, end: 480)
        let incoming = ServiceQuietHours(enabled: true, start: 1320, end: 420)
        var state = ServiceSettingsFeature.State(service: "service", device: "client-1", supportsNotifications: true)
        state.notifications = ServiceNotificationSettings(permission: .authorized, delivery: .registered, quietHours: initial)
        state.baseline = initial; state.draft = initial
        let store = TestStore(initialState: state) { ServiceSettingsFeature() } withDependencies: {
            $0.serviceSettings.save = { _, _ in Issue.record("A conflicted draft was persisted") }
        }
        await store.send(.quietStart(1260)) { $0.draft.start = 1260; $0.editing = true }
        let update = ServiceNotificationSettings(permission: .authorized, delivery: .registering, quietHours: incoming)
        await store.send(.updated(update)) { $0.notifications = update; $0.conflict = true }
        await store.send(.save)
        #expect(store.state.draft.start == 1260)
        #expect(!store.state.canSave)
        await store.send(.useCurrent) {
            $0.baseline = incoming; $0.draft = incoming; $0.editing = false; $0.conflict = false
        }
    }

    @Test func completionFromClosedSettingsCannotReplaceFreshConnectionState() async {
        var state = ServiceSettingsFeature.State(service: "service", device: "client-1", supportsNotifications: false)
        state.checking = true
        let store = TestStore(initialState: state) { ServiceSettingsFeature() }
        await store.send(.disappeared) { $0.generation = 1; $0.checking = false }
        await store.send(.checked(0))
        await store.send(.failed("old failure", 0))
        #expect(!store.state.connected)
        #expect(store.state.error == nil)
    }
}

@MainActor struct MacSelectionSettingsTests {
    @Test func selectionFailurePreservesActiveMacAndUnknownChoiceDoesNothing() async {
        let a = UUID(), b = UUID()
        var state = ServiceSettingsFeature.State(service: "service", device: "phone", supportsNotifications: false)
        state.macs = [.init(id: a, macID: "mac-a"), .init(id: b, macID: "mac-b")]
        state.selectedMacID = a
        let store = TestStore(initialState: state) { ServiceSettingsFeature() } withDependencies: {
            $0.serviceSettings.selectMac = { id in
                #expect(id == b)
                throw Failure.offline
            }
        }
        await store.send(.selectMac(UUID()))
        await store.send(.selectMac(a))
        await store.send(.selectMac(b)) { $0.selectingMacID = b }
        await store.receive(\.macSelectionFailed) { $0.selectingMacID = nil; $0.macError = "The Mac is offline." }
        #expect(store.state.selectedMacID == a)
    }
    @Test func completionAfterClosingCannotSelectAnotherMac() async {
        let a = UUID(), b = UUID()
        var state = ServiceSettingsFeature.State(service: "service", device: "phone", supportsNotifications: false)
        state.macs = [.init(id: a, macID: "mac-a"), .init(id: b, macID: "mac-b")]
        state.selectedMacID = a; state.selectingMacID = b
        let store = TestStore(initialState: state) { ServiceSettingsFeature() }
        await store.send(.disappeared) { $0.generation = 1; $0.selectingMacID = nil }
        await store.send(.macSelected(b, 0))
        await store.send(.macSelectionFailed("stale", 0))
        #expect(store.state.selectedMacID == a)
        #expect(store.state.macError == nil)
    }
    enum Failure: LocalizedError { case offline; var errorDescription: String? { "The Mac is offline." } }
}

@MainActor struct AdditionalMacSettingsTests {
    @Test func linkingAndDismissingAnotherMacNeverLogsOutOrRestoresExistingAccount() async {
        let gate = PairingCleanupGate()
        let store = TestStore(initialState: ServiceSettingsFeature.State(service: "service", device: "phone", supportsNotifications: false)) {
            ServiceSettingsFeature()
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
