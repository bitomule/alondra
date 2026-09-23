import HighlightSwift
import SwiftUI

/// Colours a file's diff lines with highlight.js, by the language its extension names. Lines it
/// cannot place stay plain, so a failure never hides code.
public enum DiffHighlighter {
    public static func highlight(_ file: DiffFile, dark: Bool) async -> [String: AttributedString] {
        let code = file.lines.filter { $0.kind != .hunk && $0.kind != .note }
        guard !code.isEmpty, let language = language(for: file.path) else { return [:] }
        let text = code.map(\.text).joined(separator: "\n")
        let colors: HighlightColors = dark ? .dark(.github) : .light(.github)
        guard let coloured = try? await Highlight().request(text, mode: .languageAliasIgnoreIllegal(language), colors: colors), !coloured.isUndefined else { return [:] }
        let lines = split(coloured.attributedText)
        var out: [String: AttributedString] = [:]
        for (line, highlighted) in zip(code, lines) {
            out[line.id] = restoringIndent(highlighted, of: line.text)
        }
        return out
    }

    /// highlight.js knows most file extensions as aliases; a few common names need a hint.
    public static func language(for path: String) -> String? {
        let name = (path as NSString).lastPathComponent
        let named = ["Makefile": "makefile", "Dockerfile": "dockerfile", "BUILD.bazel": "python", "MODULE.bazel": "python", "Package.swift": "swift"]
        if let known = named[name] { return known }
        let ext = (name as NSString).pathExtension.lowercased()
        let aliases = ["bzl": "python", "xcstrings": "json", "plist": "xml", "entitlements": "xml", "jsonl": "json", "mdx": "markdown"]
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

    public static func restoringIndent(_ highlighted: AttributedString, of raw: String) -> AttributedString? {
        let plain = String(highlighted.characters)
        guard raw.hasSuffix(plain) || plain == raw.trimmingCharacters(in: .whitespaces) else { return nil }
        let lost = String(raw.prefix(raw.count - plain.count))
        return lost.isEmpty ? highlighted : AttributedString(lost) + highlighted
    }
}

public struct DiffFileHeader: View {
    let file: DiffFile

    public init(file: DiffFile) {
        self.file = file
    }

    public var body: some View {
        HStack {
            Text(file.path)
                .font(.caption.monospaced().weight(.semibold))
                .lineLimit(1)
                .truncationMode(.head)
            Spacer()
            Text(verbatim: "+\(file.additions)")
                .foregroundStyle(.green)
            Text(verbatim: "−\(file.deletions)")
                .foregroundStyle(.red)
        }
        .font(.caption2.monospaced())
        .padding(.horizontal, 10)
        .padding(.vertical, 7)
        .background(.quaternary.opacity(0.6))
    }
}

/// One diff line: old and new numbers, the +/− mark, and the code, coloured when known.
public struct DiffCodeLine: View {
    let line: DiffLine
    let coloured: AttributedString?

    public init(line: DiffLine, coloured: AttributedString?) {
        self.line = line
        self.coloured = coloured
    }

    public var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 0) {
            number(line.old)
            number(line.new)
            Text(verbatim: mark)
                .foregroundStyle(markColor)
                .frame(width: 14)
            code
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .font(.caption.monospaced())
        .padding(.vertical, 1)
        .background(background)
    }

    @ViewBuilder
    private var code: some View {
        switch line.kind {
        case .hunk, .note:
            Text(verbatim: line.text).foregroundStyle(.secondary)
        default:
            if let coloured { Text(coloured) } else { Text(verbatim: line.text) }
        }
    }

    private var mark: String {
        switch line.kind {
        case .added: "+"
        case .removed: "−"
        default: ""
        }
    }

    private var markColor: Color {
        line.kind == .added ? .green : .red
    }

    private var background: Color {
        switch line.kind {
        case .added: .green.opacity(0.14)
        case .removed: .red.opacity(0.14)
        case .hunk: .blue.opacity(0.08)
        case .context, .note: .clear
        }
    }

    private func number(_ n: Int?) -> some View {
        Text(verbatim: n.map(String.init) ?? "")
            .foregroundStyle(.tertiary)
            .frame(width: 36, alignment: .trailing)
            .padding(.trailing, 4)
    }
}

/// The review's line comments in diff order, ready for `ReviewExport`.
public func orderedComments(_ comments: [String: String], in files: [DiffFile]) -> [(anchor: String, text: String)] {
    var seen = Set<String>()
    return files.flatMap(\.lines).compactMap { line in
        guard let anchor = line.anchor, let text = comments[anchor], seen.insert(anchor).inserted else { return nil }
        return (anchor, text)
    }
}
