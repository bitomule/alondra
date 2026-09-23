import Foundation

/// The open questions split between the inbox and the PR cards they belong to: a question
/// about a PR whose card is waiting is answered inside that card, never as a card of its own.
public struct InboxGrouping: Sendable {
    public let loose: [Question]
    public let byPullRequest: [UInt64: [Question]]

    public init(questions: [Question], pullRequests: [PullRequest]) {
        let cards = Set(pullRequests.map(\.id))
        var loose: [Question] = []
        var byPullRequest: [UInt64: [Question]] = [:]
        for question in questions {
            if let pr = question.prId, cards.contains(pr) {
                byPullRequest[pr, default: []].append(question)
            } else {
                loose.append(question)
            }
        }
        self.loose = loose
        self.byPullRequest = byPullRequest
    }

    public static let empty = InboxGrouping(questions: [], pullRequests: [])
}
