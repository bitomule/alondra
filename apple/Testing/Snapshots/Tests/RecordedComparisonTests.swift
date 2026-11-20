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
