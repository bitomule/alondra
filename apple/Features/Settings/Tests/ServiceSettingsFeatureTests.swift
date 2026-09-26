import AlondraSettingsFeature
import ComposableArchitecture
import Testing

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
