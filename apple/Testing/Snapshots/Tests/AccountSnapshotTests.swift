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
        let store = signIn { $0.showsAccount = true }
        assertScreen([.light, .dark], inWindow: true) { ScreenFrame(height: 852) { SignInView(store: store, demo: {}) } }
    }

    func test_signIn_spanish() {
        let store = signIn { $0.showsAccount = true }
        assertScreen([.light], language: .spanish, inWindow: true) { ScreenFrame(height: 852) { SignInView(store: store, demo: {}) } }
    }

    // MARK: Find your Mac

    private let studio = FoundMac(id: "Studio", name: "Studio de David", paired: false)
    private let mini = FoundMac(id: "Mini", name: "Mac mini", paired: true)


    func test_findMac_twoMacs() {
        let store = signIn { $0.findMac.macs = [studio, mini] }
        assertScreen([.light, .dark], inWindow: true) { ScreenFrame(height: 852) { SignInView(store: store, demo: {}) } }
    }

    func test_findMac_twoMacs_spanish() {
        let store = signIn { $0.findMac.macs = [studio, mini] }
        assertScreen([.light], language: .spanish, inWindow: true) { ScreenFrame(height: 852) { SignInView(store: store, demo: {}) } }
    }

    func test_findMac_aDeniedRequestStaysOnTheList() {
        let store = signIn {
            $0.findMac.macs = [studio]
            $0.findMac.error = "The Mac didn’t allow this device."
        }
        assertScreen([.light], inWindow: true) { ScreenFrame(height: 852) { SignInView(store: store, demo: {}) } }
    }

    func test_findMac_comparingTheCode() {
        let store = signIn { $0.findMac.step = .comparing(DirectPairingCode(code: "4F2A91", macName: "Studio de David")) }
        assertScreen([.light, .dark], inWindow: true) { ScreenFrame(height: 852) { SignInView(store: store, demo: {}) } }
    }

    func test_findMac_comparingTheCode_spanish() {
        let store = signIn { $0.findMac.step = .comparing(DirectPairingCode(code: "4F2A91", macName: "Studio de David")) }
        assertScreen([.light], language: .spanish, inWindow: true) { ScreenFrame(height: 852) { SignInView(store: store, demo: {}) } }
    }

    func test_findMac_fromSettings() {
        let store = signIn({ $0.findMac.macs = [studio, mini] }, additionalMac: true)
        assertScreen([.light], inWindow: true) { ScreenFrame(height: 852) { SignInView(store: store) } }
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

    /// Paired on the local network: no account, no notifications, each Mac with how it answers.
    private func localSettings() -> StoreOf<SettingsFeature> {
        var state = SettingsFeature.State(service: "mac-9c1e44aa", device: "phone-7f3a91c2d4e5", supportsNotifications: false,
                                          usesAccount: false)
        let ids = (1...3).map { UUID(uuidString: "00000000-0000-0000-0000-00000000000\($0)")! }
        state.macs = [
            MacChoice(id: ids[0], macID: "mac-9c1e44aa", name: "Studio de David", local: true),
            MacChoice(id: ids[1], macID: "mac-b27f03d9", name: "Mac mini", local: true),
            MacChoice(id: ids[2], macID: "mac-c55e10aa", name: "MacBook Pro", local: true),
        ]
        state.selectedMacID = ids[0]
        state.macStatuses = [ids[0]: .route(.localNetwork), ids[1]: .route(.tailscale), ids[2]: .problem(.macNotOnNetwork)]
        return Store(initialState: state) { EmptyReducer<SettingsFeature.State, SettingsFeature.Action>() }
    }

    func test_settings_macsPairedOnTheLocalNetwork() {
        let store = localSettings()
        assertScreen([.light, .dark], inWindow: true) { ScreenFrame(height: 1000) { SettingsView(store: store) } }
    }

    func test_settings_macsPairedOnTheLocalNetwork_spanish() {
        let store = localSettings()
        assertScreen([.light], language: .spanish, inWindow: true) { ScreenFrame(height: 1000) { SettingsView(store: store) } }
    }

    func test_settings_failuresStayInTheirRows() {
        let store = settings {
            $0.macError = .macNotAnswering
            $0.error = "Your notification settings couldn’t be saved. Try again."
        }
        assertScreen([.light], inWindow: true) { ScreenFrame(height: 1000) { SettingsView(store: store) } }
    }

    func test_settings_failuresStayInTheirRows_spanish() {
        let store = settings {
            $0.macError = .macNotAnswering
            $0.error = "No se pudieron guardar tus ajustes de notificaciones. Inténtalo de nuevo."
        }
        assertScreen([.light], language: .spanish, inWindow: true) { ScreenFrame(height: 1000) { SettingsView(store: store) } }
    }

    func test_settings_accessibilitySize() {
        let store = settings()
        assertScreen([.ax3], inWindow: true) { ScreenFrame(height: 1000) { SettingsView(store: store) } }
    }

    // MARK: Connection lost

    private func lost(_ problem: ConnectionProblem?) -> some View {
        ScreenFrame(height: 700) {
            ConnectionLostView(problem: problem, retry: {}, signIn: {}, settings: {}, signOut: {})
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

    func test_connectionLost_macIdentityChanged() {
        assertScreen([.light], inWindow: true) { lost(.macIdentityChanged) }
    }

    func test_connectionLost_macIdentityChanged_spanish() {
        assertScreen([.light], language: .spanish, inWindow: true) { lost(.macIdentityChanged) }
    }

    func test_connectionLost_signOutIncomplete() {
        assertScreen([.light, .dark], inWindow: true) { lost(.signOutIncomplete) }
    }

    func test_connectionLost_signOutIncomplete_spanish() {
        assertScreen([.light], language: .spanish, inWindow: true) { lost(.signOutIncomplete) }
    }
}
