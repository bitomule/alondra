import AlondraContracts
import Foundation

public struct DocumentCommentUseCase: Sendable {
    public init() {}
    public func updating(_ comments: [ReviewComment], anchor: String, quote: String, text: String, positions: [String: String]) -> [ReviewComment] {
        var result = comments
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if let index = result.firstIndex(where: { $0.anchor == anchor }) {
            if text.isEmpty { result.remove(at: index) }
            else { result[index].text = text }
        } else if !text.isEmpty {
            result.append(ReviewComment(anchor: anchor, quote: quote, text: text))
        }
        return result.sorted {
            (positions[$0.anchor] ?? $0.anchor).compare(positions[$1.anchor] ?? $1.anchor, options: .numeric) == .orderedAscending
        }
    }
}
