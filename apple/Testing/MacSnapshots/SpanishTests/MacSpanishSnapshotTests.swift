import AlondraContract
import AlondraFeatures
import AppKit
import ComposableArchitecture
import SnapshotTesting
import SwiftUI
import XCTest

/// The Mac screens of local pairing in Spanish, drawn on macOS with the Mac app's own strings.
@MainActor
final class MacSpanishSnapshotTests: XCTestCase {
    private func assertSpanish<Content: View>(
        size: CGSize, file: StaticString = #filePath, testName: String = #function, line: UInt = #line,
        @ViewBuilder _ content: () -> Content
    ) throws {
        try MacCatalogBundle.installSpanish()
        let view = content()
            .environment(\.locale, Locale(identifier: "es_ES"))
            .background(Color(nsColor: .windowBackgroundColor))
        let hosting = NSHostingView(rootView: view)
        let window = NSWindow(contentRect: CGRect(origin: .zero, size: size), styleMask: [.borderless], backing: .buffered, defer: false)
        window.appearance = NSAppearance(named: .aqua)
        window.contentView = hosting
        hosting.frame = CGRect(origin: .zero, size: size)
        hosting.layoutSubtreeIfNeeded()
        assertAppSnapshot(of: hosting as NSView, as: .image(perceptualPrecision: 0.98, size: size),
                          named: "light_es", file: file, testName: testName, line: line)
    }

    func test_pairingRequest_asksWithTheCode() throws {
        try assertSpanish(size: CGSize(width: 440, height: 300)) {
            MacPairingRequestSheet(request: PairingRequest(id: "p-1", name: "iPhone de David", code: "4F2A91"),
                                   error: nil, decide: { _ in })
        }
    }

    func test_settings_thisMacItsPairedMacsAndItsDevices() throws {
        var state = SettingsFeature.State(service: "127.0.0.1", device: "mac", supportsNotifications: false, usesAccount: false)
        let ids = (1...3).map { UUID(uuidString: "00000000-0000-0000-0000-00000000000\($0)")! }
        state.macs = [
            MacChoice(id: ids[0], macID: "this-mac", name: "Este Mac"),
            MacChoice(id: ids[1], macID: "mac-b27f03d9", name: "Mac mini", local: true),
            MacChoice(id: ids[2], macID: "mac-c55e10aa", name: "MacBook Pro", local: true),
        ]
        state.selectedMacID = ids[0]
        state.showsSignOutWithMacs = true
        state.macStatuses = [ids[0]: .route(.thisMac), ids[1]: .route(.localNetwork), ids[2]: .problem(.macNotOnNetwork)]
        state.devices = [
            TrustedDevice(id: "phone-1", name: "iPhone de David", origin: .direct),
            TrustedDevice(id: "phone-2", name: "iPad", origin: .relay),
        ]
        let store = Store(initialState: state) { EmptyReducer<SettingsFeature.State, SettingsFeature.Action>() }
        try assertSpanish(size: CGSize(width: 560, height: 800)) {
            MacSettingsView(store: store) { _ in EmptyView() }
        }
    }
}
