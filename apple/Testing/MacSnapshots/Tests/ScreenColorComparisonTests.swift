import AppKit
import SnapshotTesting
import XCTest

/// The delivery card captured with a 5K display as the main screen, kept as it was taken, against
/// its reference recorded with the laptop's own: a fifth of its pixels moved a few ΔE and nothing else.
@MainActor
final class ScreenColorComparisonTests: XCTestCase {
    func test_anotherMainScreenDoesNotFailACaptureButAMovedLineDoes() throws {
        let strict = Snapshotting<NSImage, NSImage>.image(perceptualPrecision: 0.98).diffing
        let comparison = Snapshotting<NSImage, NSImage>.image(perceptualPrecision: screenColorPerceptualPrecision).diffing

        let reference = try image(at: "Tests/__Snapshots__/MacWorkspaceSnapshotTests/test_card_deliveryWithItsApprovedPlan.dark.png")
        let otherScreen = try image(at: "Fixtures/delivery-card-other-main-screen.png")

        XCTAssertNotNil(strict.diff(reference, otherScreen), "the capture from another screen must exercise the comparison")
        XCTAssertNil(comparison.diff(reference, otherScreen), "the colours of another main screen must not fail a capture")
        XCTAssertNotNil(comparison.diff(reference, try movingRows(of: reference, rows: 200..<400, by: 14)), "a line that moved must fail")
    }

    private func image(at path: String) throws -> NSImage {
        let root = URL(fileURLWithPath: try XCTUnwrap(ProcessInfo.processInfo.environment["TEST_SRCDIR"]))
            .appendingPathComponent("_main/apple/Testing/MacSnapshots")
        return try XCTUnwrap(NSImage(data: Data(contentsOf: root.appendingPathComponent(path))))
    }

    private func movingRows(of image: NSImage, rows: Range<Int>, by offset: Int) throws -> NSImage {
        let source = try XCTUnwrap(image.cgImage(forProposedRect: nil, context: nil, hints: nil))
        let context = try XCTUnwrap(CGContext(
            data: nil, width: source.width, height: source.height, bitsPerComponent: 8, bytesPerRow: 0,
            space: try XCTUnwrap(source.colorSpace), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        ))
        context.draw(source, in: CGRect(x: 0, y: 0, width: source.width, height: source.height))
        let band = try XCTUnwrap(source.cropping(to: CGRect(x: 0, y: rows.lowerBound, width: source.width, height: rows.count)))
        context.draw(band, in: CGRect(x: 0, y: source.height - rows.upperBound - offset, width: source.width, height: rows.count))
        return NSImage(cgImage: try XCTUnwrap(context.makeImage()), size: image.size)
    }
}
