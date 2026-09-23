import Foundation

public struct ReviewComment: Identifiable, Hashable, Sendable {
    public let id: UUID
    public let anchor: String
    public let quote: String
    public var text: String

    public init(id: UUID = UUID(), anchor: String, quote: String, text: String) {
        self.id = id
        self.anchor = anchor
        self.quote = quote
        self.text = text
    }
}

public enum ReviewFeedback {
    public static let quoteLimit = 200

    public static func quote(_ source: String) -> String {
        let collapsed = source.split(whereSeparator: \.isWhitespace).joined(separator: " ")
        guard collapsed.count > quoteLimit else { return collapsed }
        return collapsed.prefix(quoteLimit - 1).trimmingCharacters(in: .whitespaces) + "…"
    }

    public static func format(_ comments: [ReviewComment], general: String, author: String) -> String {
        let general = general.trimmingCharacters(in: .whitespacesAndNewlines)
        let notes = comments.filter { !$0.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
        guard !notes.isEmpty else { return general }
        var parts = [author.isEmpty ? "Comments:" : "Comments from \(author):"]
        for (index, comment) in notes.enumerated() {
            let body = comment.text
                .trimmingCharacters(in: .whitespacesAndNewlines)
                .components(separatedBy: "\n")
                .map { "   " + $0 }
                .joined(separator: "\n")
            parts.append("\(index + 1). On \"\(quote(comment.quote))\":\n\(body)")
        }
        if !general.isEmpty { parts.append(general) }
        return parts.joined(separator: "\n\n")
    }
}
