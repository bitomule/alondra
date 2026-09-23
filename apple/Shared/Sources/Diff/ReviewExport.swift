import Foundation

/// Line comments and a general note in the same shape tuicr exports, so the node reads a review
/// the same way whichever client it came from.
public enum ReviewExport {
    public static func text(comments: [(anchor: String, text: String)], general: String) -> String {
        var out: [String] = []
        if !comments.isEmpty {
            out.append("I reviewed your code and have the following comments. Please address them.")
            out.append(comments.enumerated().map { "\($0.offset + 1). `\($0.element.anchor)` - \($0.element.text)" }.joined(separator: "\n"))
        }
        let note = general.trimmingCharacters(in: .whitespacesAndNewlines)
        if !note.isEmpty { out.append(note) }
        return out.joined(separator: "\n\n")
    }

    /// Comments keyed by anchor, in the order their lines appear in the diff.
    public static func ordered(_ comments: [String: String], in files: [DiffFile]) -> [(anchor: String, text: String)] {
        var seen = Set<String>()
        return files.flatMap(\.lines).compactMap { line in
            guard let anchor = line.anchor, let text = comments[anchor], seen.insert(anchor).inserted else { return nil }
            return (anchor, text)
        }
    }
}
