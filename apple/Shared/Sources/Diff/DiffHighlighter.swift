import Foundation
import HighlightSwift

/// Colours a file's diff lines with highlight.js, by the language its name implies. Lines it
/// cannot place stay plain, so a failure never hides code.
public enum DiffHighlighter {
    #if !VIEWS_ONLY
    public static func highlight(_ file: DiffFile, dark: Bool) async -> [String: AttributedString] {
        let code = file.lines.filter { $0.kind != .hunk && $0.kind != .note }
        guard !code.isEmpty, let language = language(for: file.path) else { return [:] }
        let text = code.map(\.text).joined(separator: "\n")
        guard let coloured = await colour(text, language: language, dark: dark) else { return [:] }
        var out: [String: AttributedString] = [:]
        for (line, highlighted) in zip(code, split(coloured)) {
            out[line.id] = restoringIndent(highlighted, of: line.text)
        }
        return out
    }
    #endif

    /// Nil when highlight.js does not know the language, so the caller keeps the plain text.
    public static func colour(_ text: String, language: String, dark: Bool) async -> AttributedString? {
        let colors: HighlightColors = dark ? .dark(.github) : .light(.github)
        guard let coloured = try? await Highlight().request(text, mode: .languageAliasIgnoreIllegal(language), colors: colors), !coloured.isUndefined else { return nil }
        return coloured.attributedText
    }

    public static let aliases = ["bzl": "python", "xcstrings": "json", "plist": "xml", "entitlements": "xml", "jsonl": "json", "mdx": "markdown"]

    /// highlight.js knows most file extensions as aliases; a few names need a hint.
    public static func language(for path: String) -> String? {
        let name = (path as NSString).lastPathComponent
        let named = ["Makefile": "makefile", "Dockerfile": "dockerfile", "BUILD.bazel": "python", "MODULE.bazel": "python", "Package.swift": "swift"]
        if let known = named[name] { return known }
        let ext = (name as NSString).pathExtension.lowercased()
        return aliases[ext] ?? (ext.isEmpty ? nil : ext)
    }

    public static func split(_ text: AttributedString) -> [AttributedString] {
        var lines: [AttributedString] = []
        var start = text.startIndex
        for index in text.characters.indices where text.characters[index] == "\n" {
            lines.append(AttributedString(text[start..<index]))
            start = text.characters.index(after: index)
        }
        lines.append(AttributedString(text[start..<text.endIndex]))
        return lines
    }

    /// highlight.js trims the text it is given; the leading whitespace it drops is put back.
    public static func restoringIndent(_ highlighted: AttributedString, of raw: String) -> AttributedString? {
        let plain = String(highlighted.characters)
        guard raw.hasSuffix(plain) else { return nil }
        let lost = String(raw.prefix(raw.count - plain.count))
        guard lost.allSatisfy(\.isWhitespace) else { return nil }
        return lost.isEmpty ? highlighted : AttributedString(lost) + highlighted
    }
}
