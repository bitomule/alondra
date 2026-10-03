import AlondraContract
import AlondraFeed
import AlondraiOSUI
import ComposableArchitecture
import Foundation
import SnapshotTesting
import SwiftUI
import UIKit
import XCTest

/// The instant relative times in screen snapshots are measured from: two minutes after the
/// latest timestamp in the shared fixtures.
let screenReferenceDate = Date(timeIntervalSince1970: 1_790_762_400)

/// The daemon's own rendered views, read from the shared fixtures.
enum ScreenFixtures {
    static func document(_ name: String, editing edit: (inout [String: Any]) -> Void = { _ in }) throws -> ViewDocument {
        let environment = ProcessInfo.processInfo.environment
        let root = try XCTUnwrap(environment["TEST_SRCDIR"])
        let url = URL(fileURLWithPath: root).appendingPathComponent("_main/fixtures/views/\(name).json")
        var object = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any])
        edit(&object)
        return try ContractCodec.decode(ViewDocument.self, from: JSONSerialization.data(withJSONObject: object))
    }
}

/// An idle feed: the screen starts observing on appear and nothing arrives, so the state the test
/// built is the state captured.
func idleViewsClient(_ client: inout ViewsClient) {
    client.observe = { _ in AsyncThrowingStream { _ in } }
    client.fetchContent = { _ in "" }
}

/// A whole screen on an iPhone 17 Pro, inside the navigation stack it is pushed in, in light, dark
/// and accessibility size 3, English and Spanish.
@MainActor
func assertScreenVariants<Content: View>(
    inNavigationStack: Bool = true,
    file: StaticString = #filePath,
    testName: String = #function,
    line: UInt = #line,
    @ViewBuilder _ content: () -> Content
) {
    let view = content()
    for language in [SnapshotLanguage.english, .spanish] {
        for variant in SnapshotVariant.allCases {
            let suffix = language == .english ? variant.suffix : "\(variant.suffix)_\(language.rawValue.prefix(2))"
            let screen = AnyView(
                Group { if inNavigationStack { NavigationStack { view } } else { view } }
                    .environment(\.locale, language.locale)
                    .environment(\.calendar, language.calendar)
                    .environment(\.referenceDate, screenReferenceDate)
                    .dynamicTypeSize(variant.dynamicType)
            )
            assertAppSnapshot(
                of: screen,
                as: .image(
                    drawHierarchyInKeyWindow: true,
                    perceptualPrecision: 0.98,
                    layout: .fixed(width: 393, height: 852),
                    traits: .init(userInterfaceStyle: variant.style)
                ),
                named: suffix,
                file: file,
                testName: testName,
                line: line
            )
        }
    }
}
