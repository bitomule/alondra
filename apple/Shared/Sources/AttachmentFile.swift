import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

/// images larger than Claude looks at are scaled down to save the upload.
public enum AttachmentFile {
    public static let maxImageEdge = 2576

    private static let readable: Set<String> = ["public.png", "public.jpeg", "com.compuserve.gif", "org.webmproject.webp"]

    public static func prepare(_ data: Data, name: String) -> (data: Data, name: String, isImage: Bool) {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
              let type = CGImageSourceGetType(source) as String?,
              UTType(type)?.conforms(to: .image) == true,
              CGImageSourceGetCount(source) > 0
        else {
            return (data, name, false)
        }
        let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any]
        let width = properties?[kCGImagePropertyPixelWidth] as? Int ?? 0
        let height = properties?[kCGImagePropertyPixelHeight] as? Int ?? 0
        let tooLarge = max(width, height) > maxImageEdge
        if readable.contains(type) && (!tooLarge || type == "com.compuserve.gif") {
            return (data, name, true)
        }
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: min(max(width, height), maxImageEdge),
        ]
        guard let image = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) else {
            return (data, name, true)
        }
        let keepsPNG = type == "public.png"
        let out = NSMutableData()
        let outType = (keepsPNG ? UTType.png : UTType.jpeg).identifier as CFString
        guard let destination = CGImageDestinationCreateWithData(out, outType, 1, nil) else { return (data, name, true) }
        CGImageDestinationAddImage(destination, image, [kCGImageDestinationLossyCompressionQuality: 0.85] as CFDictionary)
        guard CGImageDestinationFinalize(destination) else { return (data, name, true) }
        let stem = (name as NSString).deletingPathExtension
        return (out as Data, stem + (keepsPNG ? ".png" : ".jpg"), true)
    }

    public static func thumbnail(_ data: Data, pixels: Int = 160) -> CGImage? {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil) else { return nil }
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: pixels,
        ]
        return CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary)
    }

}
