import AlondraShared
import Observation
import SwiftUI

@MainActor
@Observable
final class ReviewDraft {
    var comments: [ReviewComment] = []
    var general = ""
    var editing: String?
    var editorText = ""
    var attachments = Attachments()
    @ObservationIgnored private var positions: [String: String] = [:]

    func feedback(author: String) -> String {
        ReviewFeedback.format(comments, general: general, author: author)
    }

    var hasFeedback: Bool {
        !feedback(author: "").isEmpty
    }

    func comment(for anchor: String) -> ReviewComment? {
        comments.first { $0.anchor == anchor }
    }

    func beginEditing(_ anchor: String) {
        editorText = comment(for: anchor)?.text ?? ""
        editing = anchor
    }

    func commit(anchor: String, quote: String, position: String? = nil) {
        if let position { positions[anchor] = position }
        let text = editorText.trimmingCharacters(in: .whitespacesAndNewlines)
        if let index = comments.firstIndex(where: { $0.anchor == anchor }) {
            if text.isEmpty {
                comments.remove(at: index)
            } else {
                comments[index].text = text
            }
        } else if !text.isEmpty {
            comments.append(ReviewComment(anchor: anchor, quote: quote, text: text))
            comments.sort { Self.order(positions[$0.anchor] ?? $0.anchor, positions[$1.anchor] ?? $1.anchor) }
        }
        cancelEditing()
    }

    func cancelEditing() {
        editing = nil
        editorText = ""
    }

    func remove(_ comment: ReviewComment) {
        comments.removeAll { $0.id == comment.id }
    }

    private static func order(_ lhs: String, _ rhs: String) -> Bool {
        lhs.compare(rhs, options: .numeric) == .orderedAscending
    }
}

@MainActor
@Observable
final class Drafts {
    @ObservationIgnored private var drafts: [String: ReviewDraft] = [:]

    func draft(for key: String) -> ReviewDraft {
        if let draft = drafts[key] { return draft }
        let draft = ReviewDraft()
        drafts[key] = draft
        return draft
    }

    func discard(_ key: String) {
        drafts[key] = nil
    }

    func discardAll() {
        drafts.removeAll()
    }
}
