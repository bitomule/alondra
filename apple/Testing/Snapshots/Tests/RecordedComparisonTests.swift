import SnapshotTesting
import UIKit
import XCTest

/// A capture is extended sRGB and a reference is a Display P3 PNG. One byte of rendering noise sends
/// the library to its perceptual comparison, which reads the capture without colour management and
/// fails every saturated pixel: a screen that is green on both sides then matches by a few percent.
@MainActor
final class RecordedComparisonTests: XCTestCase {
    func test_oneByteOfNoiseInASaturatedScreenDoesNotFailItsComparison() throws {
        let format = UIGraphicsImageRendererFormat()
        format.preferredRange = .extended
        let capture = UIGraphicsImageRenderer(size: CGSize(width: 40, height: 40), format: format).image { context in
            UIColor(red: 0.1, green: 0.7, blue: 0.2, alpha: 1).setFill()
            context.fill(CGRect(x: 0, y: 0, width: 40, height: 40))
        }
        let reference = try referenceWithOneByteChanged(of: capture)
        let strategy = Snapshotting<UIImage, UIImage>.image(perceptualPrecision: 0.98)

        let asCaptured = strategy.diffing.diff(reference, capture)
        let asRecorded = strategy.asRecorded.diffing.diff(reference, try XCTUnwrap(strategy.asRecorded.snapshot(capture).sync()))

        XCTAssertNil(asRecorded, "the capture as it would be recorded still fails: \(asRecorded?.0 ?? "")")
        XCTAssertNotNil(asCaptured, "the library compared the capture as it is without failing: the pitfall asRecorded avoids is gone")
    }

    /// The two captures that failed `TriageSnapshotTests` on a machine under load, kept as they were
    /// taken: the notice's glass text with 1.2% of its pixels a few levels off, and the buttons'
    /// native labels with a handful. The screens are the same; only the renderer's anti-aliasing moved.
    func test_triageRendererNoiseOnGlassTextAndNativeLabelsDoesNotFailButAMovedLineDoes() throws {
        let strict = Snapshotting<UIImage, UIImage>.image(precision: 1, perceptualPrecision: 0.98).diffing
        let glass = Snapshotting<UIImage, UIImage>.image(precision: glassTextPixelPrecision, perceptualPrecision: 0.98).diffing
        let label = Snapshotting<UIImage, UIImage>.image(precision: nativeLabelPixelPrecision, perceptualPrecision: 0.98).diffing

        let notice = try recording("TriageSnapshotTests/test_answersSetAsideFromAStoreThatShowedNothing.light.png")
        let noticeNoise = try fixture("set-aside-banner-glass-text.png")
        XCTAssertNotNil(strict.diffV2(notice, noticeNoise), "the retained rendering noise must exercise the comparison")
        XCTAssertNil(glass.diffV2(notice, noticeNoise), "anti-aliasing noise on glass text must not fail the notice")
        XCTAssertNotNil(glass.diffV2(notice, try movingRows(of: notice, rows: 272..<411, by: 14)), "a notice line that moved must fail")

        let stack = try recording("TriageSnapshotTests/test_aCardAloneIsAsTallAsTheFrontOfAStack.light.png")
        let stackNoise = try fixture("triage-native-button-label.png")
        XCTAssertNotNil(strict.diffV2(stack, stackNoise), "the retained rendering noise must exercise the comparison")
        XCTAssertNil(label.diffV2(stack, stackNoise), "anti-aliasing noise on native labels must not fail the stack")
        XCTAssertNotNil(label.diffV2(stack, try movingRows(of: stack, rows: 1300..<1500, by: 14)), "a label that moved must fail")
    }

    private func snapshotsRoot() throws -> URL {
        URL(fileURLWithPath: try XCTUnwrap(ProcessInfo.processInfo.environment["TEST_SRCDIR"]))
            .appendingPathComponent("_main/apple/Testing/Snapshots")
    }

    private func recording(_ path: String) throws -> UIImage {
        try XCTUnwrap(UIImage(data: Data(contentsOf: snapshotsRoot().appendingPathComponent("Tests/__Snapshots__/\(path)"))))
    }

    private func fixture(_ name: String) throws -> UIImage {
        try XCTUnwrap(UIImage(data: Data(contentsOf: snapshotsRoot().appendingPathComponent("Fixtures/\(name)"))))
    }

    private func movingRows(of image: UIImage, rows: Range<Int>, by offset: Int) throws -> UIImage {
        let original = try XCTUnwrap(image.cgImage)
        let space = try XCTUnwrap(original.colorSpace)
        var pixels = [UInt8](repeating: 0, count: original.width * original.height * 4)
        let moved: CGImage = try pixels.withUnsafeMutableBytes { raw in
            let context = try XCTUnwrap(CGContext(
                data: raw.baseAddress, width: original.width, height: original.height, bitsPerComponent: 8,
                bytesPerRow: original.width * 4, space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
            ))
            context.draw(original, in: CGRect(x: 0, y: 0, width: original.width, height: original.height))
            let rowBytes = original.width * 4
            let band = Array(raw[(rows.lowerBound * rowBytes)..<(rows.upperBound * rowBytes)])
            for (index, byte) in band.enumerated() {
                raw[rows.lowerBound * rowBytes + offset * rowBytes + index] = byte
            }
            return try XCTUnwrap(context.makeImage())
        }
        return UIImage(cgImage: moved, scale: image.scale, orientation: .up)
    }

    private func referenceWithOneByteChanged(of image: UIImage) throws -> UIImage {
        let recorded = try XCTUnwrap(image.pngData().flatMap { UIImage(data: $0, scale: image.scale)?.cgImage })
        let space = try XCTUnwrap(recorded.colorSpace)
        var pixels = [UInt8](repeating: 0, count: recorded.width * recorded.height * 4)
        let changed: CGImage = try pixels.withUnsafeMutableBytes { raw in
            let context = try XCTUnwrap(CGContext(
                data: raw.baseAddress, width: recorded.width, height: recorded.height, bitsPerComponent: 8,
                bytesPerRow: recorded.width * 4, space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
            ))
            context.draw(recorded, in: CGRect(x: 0, y: 0, width: recorded.width, height: recorded.height))
            raw[0] = raw[0] &+ 2
            return try XCTUnwrap(context.makeImage())
        }
        return UIImage(cgImage: changed, scale: image.scale, orientation: .up)
    }
}

private extension Async<UIImage> {
    func sync() -> UIImage? {
        var result: UIImage?
        run { result = $0 }
        return result
    }
}
