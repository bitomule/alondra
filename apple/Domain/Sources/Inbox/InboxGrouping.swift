import AlondraContracts
import Foundation

/// The open questions and unread reports split between the inbox and the PR cards they belong
/// to: a question or report about a PR whose card is waiting is shown inside that card, never
/// as a card of its own.
public struct InboxGrouping: Sendable {
    public let loose: [Question]
    public let byPullRequest: [UInt64: [Question]]
    public let looseReports: [Report]
    public let reportsByPullRequest: [UInt64: [Report]]

    public init(questions: [Question], reports: [Report] = [], pullRequests: [PullRequest]) {
        let cards = Set(pullRequests.map(\.id))
        let (loose, byPullRequest) = Self.split(questions, into: cards, by: \.prId)
        let (looseReports, reportsByPullRequest) = Self.split(reports, into: cards, by: \.prId)
        self.loose = loose
        self.byPullRequest = byPullRequest
        self.looseReports = looseReports
        self.reportsByPullRequest = reportsByPullRequest
    }

    private static func split<T>(_ items: [T], into cards: Set<UInt64>, by card: (T) -> UInt64?) -> ([T], [UInt64: [T]]) {
        var loose: [T] = []
        var inside: [UInt64: [T]] = [:]
        for item in items {
            if let pr = card(item), cards.contains(pr) {
                inside[pr, default: []].append(item)
            } else {
                loose.append(item)
            }
        }
        return (loose, inside)
    }

    public static let empty = InboxGrouping(questions: [], pullRequests: [])
}
