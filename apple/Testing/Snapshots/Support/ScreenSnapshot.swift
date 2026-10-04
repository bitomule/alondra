import AlondraContract
import AlondraFeatures
import AlondraFeed
import AlondraiOSUI
import ComposableArchitecture
import SnapshotTesting
import SwiftUI
import UIKit
import XCTest

/// Views the screens are drawn from: the daemon's fixtures and the populated samples beside them.
enum SampleViews {
    static func document(_ name: String) throws -> ViewDocument {
        let environment = ProcessInfo.processInfo.environment
        let root = URL(fileURLWithPath: try XCTUnwrap(environment["TEST_SRCDIR"]))
            .appendingPathComponent(environment["TEST_WORKSPACE"] ?? "_main")
        for directory in ["apple/Testing/Samples", "fixtures/views/cards"] {
            let url = root.appendingPathComponent(directory).appendingPathComponent("\(name).json")
            if let data = try? Data(contentsOf: url) { return try ContractCodec.decode(ViewDocument.self, from: data) }
        }
        throw XCTSkip("no sample named \(name)")
    }
}

extension ViewsClient {
    /// A screen's own observation delivers the view it was drawn from and nothing else, so a
    /// snapshot never depends on a load finishing.
    static func serving(_ document: ViewDocument) -> @Sendable (ViewKey) -> AsyncThrowingStream<ViewFetch, any Error> {
        { _ in AsyncThrowingStream { $0.yield(.document(document)) } }
    }
}

/// A screen the way it is on an iPhone: inside a navigation bar, as tall as the screen is, and
/// taller for the larger text sizes that make the same content longer.
struct ScreenFrame<Content: View>: View {
    let height: CGFloat
    @ViewBuilder let content: Content
    @Environment(\.dynamicTypeSize) private var textSize

    var body: some View {
        NavigationStack { content }
            .environment(\.referenceDate, snapshotReferenceDate)
            .frame(height: textSize.isAccessibilitySize ? height * 2.2 : height)
    }
}

extension ScreenFeature.State {
    /// A screen that already shows its view, as it does once the load has finished.
    static func showing(_ document: ViewDocument, key: ViewKey) -> Self {
        var state = Self(key: key)
        state.document = document
        state.phase = .loaded
        return state
    }
}

/// `assertVariants` for a whole screen: one point a pixel keep a tall screen's baseline small, and
/// `variants` leaves out the accessibility size where one screen already shows how it grows.
@MainActor
func assertScreen<Content: View>(
    _ variants: [SnapshotVariant] = SnapshotVariant.allCases,
    language: SnapshotLanguage = .english,
    inWindow: Bool = false,
    file: StaticString = #filePath,
    testName: String = #function,
    line: UInt = #line,
    @ViewBuilder _ content: () -> Content
) {
    for variant in variants {
        let suffix = language == .english ? variant.suffix : "\(variant.suffix)_\(language.rawValue.prefix(2))"
        assertAppSnapshot(
            of: screen(variant, language: language, content),
            as: .image(
                drawHierarchyInKeyWindow: inWindow,
                perceptualPrecision: 0.98,
                layout: .sizeThatFits,
                traits: UITraitCollection(traitsFrom: [
                    UITraitCollection(userInterfaceStyle: variant.style),
                    UITraitCollection(displayScale: 1),
                ])
            ),
            named: suffix,
            file: file,
            testName: testName,
            line: line
        )
    }
}
