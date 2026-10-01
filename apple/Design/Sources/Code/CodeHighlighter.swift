import Foundation

/// Colours fenced code blocks with the diff's highlighter, away from the main thread. A block
/// highlight.js cannot colour is cached as its plain text, so it is not tried again.
public enum CodeHighlighter {
    static let cache = CodeColourCache(capacity: 256)

    public static func cached(_ key: CodeColourKey) -> AttributedString? {
        cache[key]
    }

    @concurrent
    public static func colour(_ key: CodeColourKey) async -> AttributedString {
        if let cached = cache[key] { return cached }
        let trimmed = String(key.code.reversed().drop(while: \.isWhitespace).reversed())
        var text = AttributedString(key.code)
        if let coloured = await DiffHighlighter.colour(trimmed, language: key.language, dark: key.dark),
           let restored = DiffHighlighter.restoringIndent(coloured, of: trimmed) {
            text = restored
        }
        cache.store(text, for: key)
        return text
    }
}
