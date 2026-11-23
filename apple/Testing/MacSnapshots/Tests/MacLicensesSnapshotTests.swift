import AlondraFeatures
import AppKit
import SnapshotTesting
import SwiftUI
import XCTest

/// Settings › Licenses on the Mac: Alondra's license and the third-party packages, in light and dark and in Spanish.
@MainActor
final class MacLicensesSnapshotTests: XCTestCase {
    override func setUp() async throws {
        _ = NSApplication.shared
    }

    private let size = CGSize(width: 560, height: 760)

    private func assertWindow(spanish: Bool = false, variants: [(String, NSAppearance.Name)],
                              file: StaticString = #filePath, testName: String = #function, line: UInt = #line) {
        let view = AnyView(NavigationStack { LicensesScreen() }
            .background(Color(nsColor: .windowBackgroundColor))
            .environment(\.locale, spanish ? Locale(identifier: "es_ES") : .current))
        for (name, appearance) in variants {
            let hosting = NSHostingView(rootView: view)
            let window = snapshotWindow(size: size)
            defer { window.close() }
            window.appearance = NSAppearance(named: appearance)
            window.contentView = hosting
            hosting.frame = CGRect(origin: .zero, size: size)
            hosting.layoutSubtreeIfNeeded()
            assertAppSnapshot(of: hosting as NSView, as: .image(perceptualPrecision: 0.98, size: size),
                              named: spanish ? "\(name)_es" : name, file: file, testName: testName, line: line)
        }
    }

    func test_licenses() {
        assertWindow(variants: [("light", .aqua), ("dark", .darkAqua)])
    }

    func test_licenses_spanish() {
        assertWindow(spanish: true, variants: [("light", .aqua)])
    }
}
