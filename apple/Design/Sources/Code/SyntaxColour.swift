import Foundation
import HighlightSwift

/// Colours code with highlight.js. Text it cannot place stays plain, so a failure never hides code.
public enum SyntaxColour {
    /// Nil when highlight.js does not know the language, so the caller keeps the plain text.
    public static func colour(_ text: String, language: String, dark: Bool) async -> AttributedString? {
        let colors: HighlightColors = dark ? .dark(.github) : .light(.github)
        guard let coloured = try? await Highlight().request(text, mode: .languageAliasIgnoreIllegal(language), colors: colors), !coloured.isUndefined else { return nil }
        return coloured.attributedText
    }

    public static let aliases = ["bzl": "python", "xcstrings": "json", "plist": "xml", "entitlements": "xml", "jsonl": "json", "mdx": "markdown"]

    /// highlight.js trims the text it is given; the leading whitespace it drops is put back.
    public static func restoringIndent(_ highlighted: AttributedString, of raw: String) -> AttributedString? {
        let plain = String(highlighted.characters)
        guard raw.hasSuffix(plain) else { return nil }
        let lost = String(raw.prefix(raw.count - plain.count))
        guard lost.allSatisfy(\.isWhitespace) else { return nil }
        return lost.isEmpty ? highlighted : AttributedString(lost) + highlighted
    }
}
