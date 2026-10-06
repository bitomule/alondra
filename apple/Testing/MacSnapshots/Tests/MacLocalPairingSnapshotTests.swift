import AlondraContract
import AlondraFeatures
import AlondraMacWorkspace
import AppKit
import ComposableArchitecture
import SnapshotTesting
import SwiftUI
import XCTest

/// What this Mac shows about other devices: the request to allow one, and the list of the
/// devices and Macs it works with.
@MainActor
final class MacLocalPairingSnapshotTests: XCTestCase {
    override func setUp() async throws {
        _ = NSApplication.shared
    }

    private func assertWindow<Content: View>(
        size: CGSize, file: StaticString = #filePath, testName: String = #function, line: UInt = #line,
        @ViewBuilder _ content: () -> Content
    ) {
        let view = content()
        for (name, appearance) in [("light", NSAppearance.Name.aqua), ("dark", .darkAqua)] {
            let hosting = NSHostingView(rootView: view.background(Color(nsColor: .windowBackgroundColor)))
            let window = NSWindow(contentRect: CGRect(origin: .zero, size: size), styleMask: [.borderless], backing: .buffered, defer: false)
            window.appearance = NSAppearance(named: appearance)
            window.contentView = hosting
            hosting.frame = CGRect(origin: .zero, size: size)
            hosting.layoutSubtreeIfNeeded()
            assertAppSnapshot(of: hosting as NSView, as: .image(perceptualPrecision: 0.98, size: size),
                              named: name, file: file, testName: testName, line: line)
        }
    }

    func test_pairingRequest_asksWithTheCode() {
        assertWindow(size: CGSize(width: 440, height: 300)) {
            MacPairingRequestSheet(request: PairingRequest(id: "p-1", name: "iPhone de David", code: "4F2A91"),
                                   error: nil, decide: { _ in })
        }
    }

    func test_settings_thisMacItsPairedMacsAndItsDevices() {
        var state = SettingsFeature.State(service: "127.0.0.1", device: "mac", supportsNotifications: false, usesAccount: false)
        let ids = (1...3).map { UUID(uuidString: "00000000-0000-0000-0000-00000000000\($0)")! }
        state.macs = [
            MacChoice(id: ids[0], macID: "this-mac", name: "This Mac"),
            MacChoice(id: ids[1], macID: "mac-b27f03d9", name: "Mac mini", local: true),
            MacChoice(id: ids[2], macID: "mac-c55e10aa", name: "MacBook Pro", local: true),
        ]
        state.selectedMacID = ids[0]
        state.macStatuses = [ids[0]: .route(.thisMac), ids[1]: .route(.localNetwork), ids[2]: .problem(.macNotOnNetwork)]
        state.devices = [
            TrustedDevice(id: "phone-1", name: "iPhone de David", origin: .direct),
            TrustedDevice(id: "phone-2", name: "iPad", origin: .relay),
        ]
        let store = Store(initialState: state) { EmptyReducer<SettingsFeature.State, SettingsFeature.Action>() }
        assertWindow(size: CGSize(width: 560, height: 760)) {
            MacSettingsView(store: store) { _ in EmptyView() }
        }
    }
}
