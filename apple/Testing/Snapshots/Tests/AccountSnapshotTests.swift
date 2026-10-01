import AlondraContract
import AlondraDesign
import AlondraFeatures
import AlondraiOSUI
import ComposableArchitecture
import SwiftUI
import XCTest

/// Signing in, linking a Mac, the settings sheet, and the screen that takes over when the Mac
/// cannot be reached.
@MainActor
final class AccountSnapshotTests: XCTestCase {
    private func signIn(_ edit: (inout SignInFeature.State) -> Void = { _ in }, additionalMac: Bool = false)
        -> StoreOf<SignInFeature> {
        var state = SignInFeature.State(additionalMac: additionalMac)
        if !additionalMac { state.phase = .signedOut }
        edit(&state)
        return Store(initialState: state) { EmptyReducer<SignInFeature.State, SignInFeature.Action>() }
    }

    private func settings(_ edit: (inout SettingsFeature.State) -> Void = { _ in }) -> StoreOf<SettingsFeature> {
        var state = SettingsFeature.State(service: "alondra.example.com", device: "iphone-7f3a91c2d4e5", supportsNotifications: true)
        state.notifications = NotificationSettings(
            permission: .authorized, delivery: .registered, quietHours: QuietHours(enabled: true, start: 23 * 60, end: 7 * 60))
        state.draft = QuietHours(enabled: true, start: 23 * 60, end: 7 * 60)
        state.macs = [
            MacChoice(id: UUID(uuidString: "00000000-0000-0000-0000-000000000001")!, macID: "9c1e44aa7b20"),
            MacChoice(id: UUID(uuidString: "00000000-0000-0000-0000-000000000002")!, macID: "b27f03d9e611"),
        ]
        state.selectedMacID = state.macs.first?.id
        edit(&state)
        return Store(initialState: state) { EmptyReducer<SettingsFeature.State, SettingsFeature.Action>() }
    }

    // MARK: Sign in

    func test_signIn() {
        let store = signIn()
        assertScreen([.light, .dark], inWindow: true) { ScreenFrame(height: 852) { SignInView(store: store, demo: {}) } }
    }

    func test_signIn_spanish() {
        let store = signIn()
        assertScreen([.light], language: .spanish, inWindow: true) { ScreenFrame(height: 852) { SignInView(store: store, demo: {}) } }
    }

    // MARK: Pairing

    func test_pairing_beforeTheCamera() {
        let store = signIn(additionalMac: true)
        assertScreen([.light, .dark], inWindow: true) { ScreenFrame(height: 852) { PairingView(store: store) } }
    }

    func test_pairing_afterAFailedAttemptTheCameraStaysOffAndTheErrorStays() {
        let store = signIn({ $0.error = "That code has expired. Run alondra pair again." }, additionalMac: true)
        assertScreen([.light], inWindow: true) { ScreenFrame(height: 852) { PairingView(store: store) } }
    }

    func test_pairing_connected() {
        let store = signIn({ $0.phase = .approved }, additionalMac: true)
        assertScreen([.light], inWindow: true) { ScreenFrame(height: 852) { PairingView(store: store) } }
    }

    // MARK: Settings

    func test_settings() {
        let store = settings()
        assertScreen([.light, .dark], inWindow: true) { ScreenFrame(height: 1000) { SettingsView(store: store) } }
    }

    func test_settings_spanish() {
        let store = settings()
        assertScreen([.light], language: .spanish, inWindow: true) { ScreenFrame(height: 1000) { SettingsView(store: store) } }
    }

    func test_settings_alertsOffAndNothingToSelect() {
        let store = settings {
            $0.notifications = NotificationSettings(permission: .denied, delivery: .idle, quietHours: QuietHours())
            $0.draft = QuietHours()
            $0.macs = []
            $0.selectedMacID = nil
        }
        assertScreen([.light], inWindow: true) { ScreenFrame(height: 1000) { SettingsView(store: store) } }
    }

    func test_settings_accessibilitySize() {
        let store = settings()
        assertScreen([.ax3], inWindow: true) { ScreenFrame(height: 1000) { SettingsView(store: store) } }
    }

    // MARK: Connection lost

    private func lost(_ problem: ConnectionProblem?) -> some View {
        ScreenFrame(height: 700) {
            ConnectionLostView(problem: problem, retry: {}, signIn: {}, settings: {})
                .background(Palette.night)
        }
    }

    func test_connectionLost_sessionExpired() {
        assertScreen([.light, .dark], inWindow: true) { lost(.sessionExpired) }
    }

    func test_connectionLost_sessionExpired_spanish() {
        assertScreen([.light], language: .spanish, inWindow: true) { lost(.sessionExpired) }
    }

    func test_connectionLost_deviceDisconnected() {
        assertScreen([.light], inWindow: true) { lost(.deviceRevoked) }
    }
}
