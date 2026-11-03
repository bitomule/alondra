import AlondraFeatures
import SwiftUI
import XCTest

/// Settings › Licenses on the iPhone: Alondra's license and the third-party packages, each in light and dark.
@MainActor
final class LicensesSnapshotTests: XCTestCase {
    func test_licenses() {
        assertScreenVariants { LicensesScreen() }
    }
}
