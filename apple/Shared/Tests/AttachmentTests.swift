import AlondraShared
import CoreGraphics
import Foundation
import ImageIO
import Testing
import UniformTypeIdentifiers

@Suite struct AttachmentTests {
    static func image(width: Int, height: Int, as type: UTType) -> Data {
        let context = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
        context.setFillColor(CGColor(red: 0.2, green: 0.7, blue: 0.8, alpha: 1))
        context.fill(CGRect(x: 0, y: 0, width: width, height: height))
        let out = NSMutableData()
        let destination = CGImageDestinationCreateWithData(out, type.identifier as CFString, 1, nil)!
        CGImageDestinationAddImage(destination, context.makeImage()!, nil)
        CGImageDestinationFinalize(destination)
        return out as Data
    }

    private static func pixels(_ data: Data) -> (type: String, width: Int, height: Int) {
        let source = CGImageSourceCreateWithData(data as CFData, nil)!
        let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as! [CFString: Any]
        return (CGImageSourceGetType(source)! as String, properties[kCGImagePropertyPixelWidth] as! Int, properties[kCGImagePropertyPixelHeight] as! Int)
    }

    @Test func anIPhonePhotoBecomesAJPEGClaudeCanRead() {
        let heic = Self.image(width: 400, height: 300, as: .heic)
        let prepared = AttachmentFile.prepare(heic, name: "IMG_0042.HEIC")
        #expect(prepared.name == "IMG_0042.jpg")
        #expect(prepared.isImage)
        #expect(Self.pixels(prepared.data).type == UTType.jpeg.identifier)
    }

    @Test func aScreenshotLargerThanClaudeLooksAtIsScaledDownAndStaysPNG() {
        let png = Self.image(width: 1320, height: 2868, as: .png)
        let prepared = AttachmentFile.prepare(png, name: "Captura.png")
        let out = Self.pixels(prepared.data)
        #expect(prepared.name == "Captura.png")
        #expect(out.type == UTType.png.identifier)
        #expect(out.height == AttachmentFile.maxImageEdge)
        #expect(out.width < 1320)
    }

    @Test func aSmallReadableImageAndAnyOtherFileGoAsTheyAre() {
        let jpeg = Self.image(width: 200, height: 100, as: .jpeg)
        #expect(AttachmentFile.prepare(jpeg, name: "a.jpg").data == jpeg)
        let pdf = Data("%PDF-1.7\n".utf8)
        let prepared = AttachmentFile.prepare(pdf, name: "plan.pdf")
        #expect(prepared.data == pdf && prepared.name == "plan.pdf" && !prepared.isImage)
    }

}
