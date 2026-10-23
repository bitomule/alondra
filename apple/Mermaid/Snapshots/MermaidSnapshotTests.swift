import AlondraMermaid
import SnapshotTesting
import SwiftUI
import UIKit
import XCTest

/// Every corpus diagram drawn in light and dark at the width of an iPhone 17 Pro, a large flowchart
/// at the width of a Mac window and one diagram at the largest accessibility text size.
@MainActor
final class MermaidSnapshotTests: XCTestCase {
    private static let iPhoneWidth: CGFloat = 393
    private static let macWidth: CGFloat = 1000

    private static var fixtures: URL {
        get throws {
            URL(fileURLWithPath: try XCTUnwrap(ProcessInfo.processInfo.environment["TEST_SRCDIR"]))
                .appendingPathComponent("_main/apple/Mermaid/Fixtures/valid")
        }
    }

    private static func corpus() throws -> [String] {
        try FileManager.default.contentsOfDirectory(atPath: fixtures.path)
            .filter { $0.hasSuffix(".mmd") }
            .map { String($0.dropLast(4)) }
            .sorted()
    }

    private static func diagram(_ name: String) throws -> Diagram {
        try Mermaid.parse(String(contentsOf: fixtures.appendingPathComponent("\(name).mmd"), encoding: .utf8))
    }

    func test_corpus() throws {
        let names = try Self.corpus()
        XCTAssertGreaterThanOrEqual(names.count, 40)
        for name in names {
            let layout = Mermaid.layout(try Self.diagram(name), typography: MermaidTypography(size: 14))
            for style in [UIUserInterfaceStyle.light, .dark] {
                assertAppSnapshot(
                    of: Self.frame(layout, theme: .sample(style, size: 14), width: Self.iPhoneWidth),
                    as: Self.image(style), named: "\(name).\(style == .dark ? "dark" : "light")"
                )
            }
        }
    }

    func test_largeFlowchartAtMacWidth() throws {
        let layout = Mermaid.layout(try Self.diagram("flow_thirty_nodes"), typography: MermaidTypography(size: 13))
        for style in [UIUserInterfaceStyle.light, .dark] {
            assertAppSnapshot(
                of: Self.frame(layout, theme: .sample(style, size: 13), width: Self.macWidth),
                as: Self.image(style), named: style == .dark ? "dark" : "light"
            )
        }
    }

    func test_largestAccessibilityText() throws {
        let traits = UITraitCollection(preferredContentSizeCategory: .accessibilityExtraExtraExtraLarge)
        let size = UIFontMetrics(forTextStyle: .body).scaledValue(for: 14, compatibleWith: traits).rounded()
        let layout = Mermaid.layout(try Self.diagram("flow_card_routing"), typography: MermaidTypography(size: size))
        assertAppSnapshot(
            of: Self.frame(layout, theme: .sample(.light, size: size), width: Self.iPhoneWidth),
            as: Self.image(.light), named: "light"
        )
    }

    /// The diagram at its natural size, scaled down only when wider than the screen.
    private static func frame(_ layout: MermaidLayout, theme: MermaidTheme, width: CGFloat) -> AnyView {
        let available = width - 32
        let scale = min(1, available / max(layout.size.width, 1))
        return AnyView(
            MermaidCanvas(layout: layout, theme: theme)
                .scaleEffect(scale, anchor: .topLeading)
                .frame(width: layout.size.width * scale, height: layout.size.height * scale, alignment: .topLeading)
                .padding(16)
                .frame(width: width, alignment: .leading)
                .background(theme.background)
        )
    }

    private static func image(_ style: UIUserInterfaceStyle) -> Snapshotting<AnyView, UIImage> {
        let base = Snapshotting<AnyView, UIImage>.image(
            perceptualPrecision: 0.98, layout: .sizeThatFits, traits: .init(userInterfaceStyle: style)
        )
        return Snapshotting(pathExtension: base.pathExtension, diffing: base.diffing) { value in
            base.snapshot(value).map { image in
                image.pngData().flatMap { UIImage(data: $0, scale: image.scale) } ?? image
            }
        }
    }
}

private extension MermaidTheme {
    var background: Color { labelBackground }

    static func sample(_ style: UIUserInterfaceStyle, size: CGFloat) -> MermaidTheme {
        let dark = style == .dark
        func color(_ light: UInt32, _ night: UInt32) -> Color {
            let value = dark ? night : light
            return Color(red: Double((value >> 16) & 0xFF) / 255, green: Double((value >> 8) & 0xFF) / 255, blue: Double(value & 0xFF) / 255)
        }
        return MermaidTheme(
            typography: MermaidTypography(size: size),
            text: color(0x1F2430, 0xECEFF4),
            mutedText: color(0x5C6370, 0xA7AFBE),
            nodeFill: color(0xF4F1EA, 0x2A2F3A),
            nodeStroke: color(0x8A8F99, 0x6B7385),
            edge: color(0x5C6370, 0xA7AFBE),
            labelBackground: color(0xFFFFFF, 0x1B1E25),
            clusterFill: color(0xEEF3F8, 0x222833),
            clusterStroke: color(0xB7C3D0, 0x46505F),
            noteFill: color(0xFFF4D6, 0x3A3424),
            noteStroke: color(0xD9B75F, 0x8C7640),
            accent: color(0xD9774A, 0xE58A5E)
        )
    }
}
