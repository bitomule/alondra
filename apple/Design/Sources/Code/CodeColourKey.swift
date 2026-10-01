import Foundation

/// What a coloured code block depends on: its text, its language and the colour scheme.
public struct CodeColourKey: Hashable, Sendable {
    public let code: String
    public let language: String
    public let dark: Bool

    /// Nil when there is nothing to colour: no code, or a fence that names no language.
    public init?(code: String, fence: String?, dark: Bool) {
        guard !code.allSatisfy(\.isWhitespace), let language = Self.language(fence: fence) else { return nil }
        self.code = code
        self.language = language
        self.dark = dark
    }

    static let plain: Set = ["text", "txt", "plain", "plaintext", "none", "output", "log"]

    /// The highlight.js name for a fence's info string: its first word, lowercased, without the
    /// `{.lang}` or `.lang` dressing some writers add.
    public static func language(fence: String?) -> String? {
        guard let word = fence?.split(whereSeparator: \.isWhitespace).first else { return nil }
        let name = word.lowercased().trimmingCharacters(in: CharacterSet(charactersIn: "{}."))
        guard !name.isEmpty, !plain.contains(name) else { return nil }
        return DiffHighlighter.aliases[name] ?? name
    }
}
