import CoreGraphics
import CoreText
import Foundation

/// Text sizes from CoreText with the theme's fonts, so a label's box fits what SwiftUI draws.
struct Measure {
    struct Block: Equatable {
        var lines: [String]
        var size: CGSize
        var lineHeight: CGFloat
    }

    private let typography: MermaidTypography

    init(typography: MermaidTypography) {
        self.typography = typography
    }

    func text(_ text: String, _ role: MermaidLayout.TextRole) -> Block {
        let font = Self.coreTextFont(typography.font(role))
        let lineHeight = (CTFontGetAscent(font) + CTFontGetDescent(font) + CTFontGetLeading(font)).rounded(.up)
        let lines = text.isEmpty ? [] : text.components(separatedBy: "\n")
        let width = lines.map { line -> CGFloat in
            let attributed = NSAttributedString(string: line, attributes: [NSAttributedString.Key(kCTFontAttributeName as String): font])
            let typeset = CTLineCreateWithAttributedString(attributed)
            return CGFloat(CTLineGetTypographicBounds(typeset, nil, nil, nil))
        }.max() ?? 0
        return Block(lines: lines, size: CGSize(width: width.rounded(.up), height: lineHeight * CGFloat(lines.count)), lineHeight: lineHeight)
    }

    static func coreTextFont(_ font: MermaidFont) -> CTFont {
        let system = CTFontCreateUIFontForLanguage(.system, font.size, nil)
            ?? CTFontCreateWithName("Helvetica" as CFString, font.size, nil)
        guard font.weight != .regular else { return system }
        let traits = [kCTFontWeightTrait: font.weight.coreText] as CFDictionary
        let descriptor = CTFontDescriptorCreateCopyWithAttributes(
            CTFontCopyFontDescriptor(system), [kCTFontTraitsAttribute: traits] as CFDictionary
        )
        return CTFontCreateWithFontDescriptor(descriptor, font.size, nil)
    }
}
