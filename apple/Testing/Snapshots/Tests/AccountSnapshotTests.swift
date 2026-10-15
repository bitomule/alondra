import AlondraContract
import AlondraDesign
import AlondraFeatures
import AlondraiOSUI
import ComposableArchitecture
import SnapshotTesting
import SwiftUI
import UIKit
import XCTest

/// Signing in, linking a Mac, the settings sheet, and the screen that takes over when the Mac
/// cannot be reached.
@MainActor
final class AccountSnapshotTests: XCTestCase {
    private static let pairingPixelPrecision: Float = 0.9995
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
        assertScreen([.light], inWindow: true, pixelPrecision: Self.pairingPixelPrecision) { ScreenFrame(height: 852) { SignInView(store: store, demo: {}) } }
    }

    func test_pairingComparisonAcceptsNativeLabelVariationButRejectsMissingNoticeAndMovedCard() throws {
        let root = URL(fileURLWithPath: try XCTUnwrap(ProcessInfo.processInfo.environment["TEST_SRCDIR"]))
            .appendingPathComponent("_main/apple/Testing/Snapshots")
        let reference = try XCTUnwrap(UIImage(data: Data(contentsOf: root.appendingPathComponent(
            "Tests/__Snapshots__/AccountSnapshotTests/test_findMac_aDeniedRequestStaysOnTheList.light.png"))))
        let nativeLabel = try XCTUnwrap(UIImage(data: Data(contentsOf: root.appendingPathComponent("Fixtures/pairing-native-label.png"))))
        let strict = Snapshotting<UIImage, UIImage>.image(precision: 1, perceptualPrecision: 0.98).diffing
        let local = Snapshotting<UIImage, UIImage>.image(precision: Self.pairingPixelPrecision, perceptualPrecision: 0.98).diffing
        XCTAssertEqual(reference.size, nativeLabel.size)
        XCTAssertEqual(reference.imageOrientation, nativeLabel.imageOrientation)
        XCTAssertNotNil(strict.diffV2(reference, nativeLabel), "the retained real rendering variation must exercise the comparison")
        XCTAssertNil(local.diffV2(reference, nativeLabel), "native label variation must not fail an otherwise identical screen")
        XCTAssertNotNil(local.diffV2(reference, try pairingImage(reference, removingNotice: true)), "the denied-device notice must remain visible")
        XCTAssertNotNil(local.diffV2(reference, try pairingImage(reference, removingNotice: false)), "the Mac card must stay in position")
    }

    private func pairingImage(_ image: UIImage, removingNotice: Bool) throws -> UIImage {
        let original = try XCTUnwrap(image.cgImage)
        let bytesPerPixel = try XCTUnwrap(original.bitsPerPixel.isMultiple(of: 8) ? original.bitsPerPixel / 8 : nil)
        XCTAssertEqual(original.width, 393)
        XCTAssertEqual(original.height, 852)
        let bytes = try XCTUnwrap(original.dataProvider?.data) as Data
        var changed = bytes
        let rows = removingNotice ? 432..<459 : 261..<531
        let columns = removingNotice ? 45..<332 : 25..<376
        for y in rows {
            for x in columns {
                let sourceX = removingNotice ? 30 : (x >= 33 ? x - 8 : 10)
                let source = y * original.bytesPerRow + sourceX * bytesPerPixel
                let target = y * original.bytesPerRow + x * bytesPerPixel
                changed.replaceSubrange(target..<(target + bytesPerPixel), with: bytes[source..<(source + bytesPerPixel)])
            }
        }
        let provider = try XCTUnwrap(CGDataProvider(data: changed as CFData))
        let result = try XCTUnwrap(CGImage(width: original.width, height: original.height,
            bitsPerComponent: original.bitsPerComponent, bitsPerPixel: original.bitsPerPixel,
            bytesPerRow: original.bytesPerRow, space: try XCTUnwrap(original.colorSpace),
            bitmapInfo: original.bitmapInfo, provider: provider, decode: nil,
            shouldInterpolate: original.shouldInterpolate, intent: original.renderingIntent))
        return UIImage(cgImage: result, scale: image.scale, orientation: image.imageOrientation)
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
        let store = signIn({ $0.error = "That code has expired. Run alondra device invite again." }, additionalMac: true)
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

    func test_settings_nodesAtOnce() throws {
        let limit = NodeLimitSetting(document: try ScreenFixtures.document("machine-settings-ios"))
        let store = settings { $0.nodeLimit.setting = limit }
        assertScreen([.light, .dark], inWindow: true) { ScreenFrame(height: 1100) { SettingsView(store: store) } }
    }

    func test_settings_nodesAtOnce_spanish() throws {
        let limit = NodeLimitSetting(document: try ScreenFixtures.document("machine-settings-ios-es"))
        let store = settings { $0.nodeLimit.setting = limit }
        assertScreen([.light], language: .spanish, inWindow: true) { ScreenFrame(height: 1100) { SettingsView(store: store) } }
    }

    func test_settings_permissionsAllowedAlways() throws {
        let document = try ScreenFixtures.document("machine-settings-ios")
        let store = settings {
            $0.nodeLimit.setting = NodeLimitSetting(document: document)
            $0.allowedRules.rules = AllowedRule.all(in: document)
        }
        assertScreen([.light, .dark], inWindow: true) { ScreenFrame(height: 1500) { SettingsView(store: store) } }
    }

    func test_settings_permissionsAllowedAlways_spanish() throws {
        let document = try ScreenFixtures.document("machine-settings-ios-es")
        let store = settings {
            $0.nodeLimit.setting = NodeLimitSetting(document: document)
            $0.allowedRules.rules = AllowedRule.all(in: document)
            $0.allowedRules.error = "Alondra no pudo cambiar el fichero de ajustes del repo"
        }
        assertScreen([.light], language: .spanish, inWindow: true) { ScreenFrame(height: 1500) { SettingsView(store: store) } }
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

    /// Macs reached over iroh: one straight to the Mac, one through the relay's live connection.
    private func liveSettings() -> StoreOf<SettingsFeature> {
        settings {
            $0.macs = [
                MacChoice(id: UUID(uuidString: "00000000-0000-0000-0000-000000000001")!, macID: "mac-9c1e44aa", name: "Studio de David"),
                MacChoice(id: UUID(uuidString: "00000000-0000-0000-0000-000000000002")!, macID: "mac-b27f03d9", name: "MacBook Pro"),
            ]
            $0.selectedMacID = $0.macs.first?.id
            $0.macStatuses = [$0.macs[0].id: .route(.direct), $0.macs[1].id: .route(.irohRelay)]
        }
    }

    func test_settings_macsOverTheLiveConnection() {
        let store = liveSettings()
        assertScreen([.light, .dark], inWindow: true) { ScreenFrame(height: 1000) { SettingsView(store: store) } }
    }

    func test_settings_macsOverTheLiveConnection_spanish() {
        let store = liveSettings()
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

    private func lost(_ problem: ConnectionProblem?, pairAgain: (() -> Void)? = nil) -> some View {
        ScreenFrame(height: 700) {
            ConnectionLostView(problem: problem, retry: {}, signIn: {}, settings: {}, signOut: {}, pairAgain: pairAgain,
                               localNetworkSettings: {})
                .background(Palette.night)
        }
    }

    func test_connectionLost_sessionExpired() {
        assertScreen([.light, .dark], inWindow: true) { lost(.sessionExpired) }
    }

    func test_connectionLost_sessionExpired_spanish() {
        assertScreen([.light], language: .spanish, inWindow: true) { lost(.sessionExpired) }
    }

    func test_connectionLost_accessNotConfirmed() {
        assertScreen([.light, .dark], inWindow: true, animatedPixels: true) { lost(.accessNotConfirmed, pairAgain: {}) }
    }

    func test_connectionLost_accessNotConfirmed_spanish() {
        assertScreen([.light], language: .spanish, inWindow: true, animatedPixels: true) { lost(.accessNotConfirmed, pairAgain: {}) }
    }

    func test_connectionLost_deviceDisconnected() {
        assertScreen([.light], inWindow: true) { lost(.deviceRevoked(macName: nil)) }
    }

    func test_connectionLost_deviceRevokedPairAgain() {
        assertScreen([.light, .dark], inWindow: true) { lost(.deviceRevoked(macName: "Mac Studio"), pairAgain: {}) }
    }

    func test_connectionLost_deviceRevokedPairAgain_spanish() {
        assertScreen([.light], language: .spanish, inWindow: true) { lost(.deviceRevoked(macName: "Mac Studio"), pairAgain: {}) }
    }

    func test_connectionLost_macChangedIdentity() {
        assertScreen([.light, .dark], inWindow: true, animatedPixels: true) { lost(.macChangedIdentity, pairAgain: {}) }
    }

    func test_connectionLost_macChangedIdentity_spanish() {
        assertScreen([.light], language: .spanish, inWindow: true, animatedPixels: true) { lost(.macChangedIdentity, pairAgain: {}) }
    }



    func test_connectionLost_deviceStorage() {
        assertScreen([.light, .dark], inWindow: true, animatedPixels: true) { lost(.deviceStorage) }
    }

    func test_connectionLost_deviceStorage_spanish() {
        assertScreen([.light], language: .spanish, inWindow: true, animatedPixels: true) { lost(.deviceStorage) }
    }

    func test_connectionLost_signOutIncomplete() {
        assertScreen([.light, .dark], inWindow: true) { lost(.signOutIncomplete) }
    }

    func test_connectionLost_signOutIncomplete_spanish() {
        assertScreen([.light], language: .spanish, inWindow: true) { lost(.signOutIncomplete) }
    }

    func test_connectionLost_macNotAnsweringItMayBeAsleep() {
        assertScreen([.light, .dark], inWindow: true, animatedPixels: true) { lost(.macNotOnNetwork) }
    }

    func test_connectionLost_macNotAnsweringItMayBeAsleep_spanish() {
        assertScreen([.light], language: .spanish, inWindow: true, animatedPixels: true) { lost(.macNotOnNetwork) }
    }

    func test_connectionLost_localNetworkDenied() {
        assertScreen([.light, .dark], inWindow: true) { lost(.localNetworkDenied) }
    }

    func test_connectionLost_localNetworkDenied_spanish() {
        assertScreen([.light], language: .spanish, inWindow: true) { lost(.localNetworkDenied) }
    }

    // MARK: Local Network permission

    func test_findMac_explainsBeforeTheSystemAsks() {
        let store = signIn { $0.findMac.explainsPermission = true }
        assertScreen([.light, .dark], inWindow: true) { ScreenFrame(height: 852) { SignInView(store: store, demo: {}) } }
    }

    func test_findMac_explainsBeforeTheSystemAsks_spanish() {
        let store = signIn { $0.findMac.explainsPermission = true }
        assertScreen([.light], language: .spanish, inWindow: true) { ScreenFrame(height: 852) { SignInView(store: store, demo: {}) } }
    }

    func test_findMac_permissionDenied() {
        let store = signIn { $0.findMac.permissionDenied = true }
        assertScreen([.light], inWindow: true) { ScreenFrame(height: 852) { SignInView(store: store, demo: {}) } }
    }

    func test_findMac_permissionDenied_spanish() {
        let store = signIn { $0.findMac.permissionDenied = true }
        assertScreen([.light], language: .spanish, inWindow: true) { ScreenFrame(height: 852) { SignInView(store: store, demo: {}) } }
    }
}
