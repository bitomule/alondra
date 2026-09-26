import AlondraContracts
import AlondraDomain
import Foundation
import Testing

@Suite struct InboxGroupingTests {
    private static func decode<T: Decodable>(_ json: String) throws -> T {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(T.self, from: Data(json.utf8))
    }

    private static func question(_ id: UInt64, pr: UInt64?) throws -> Question {
        let link = pr.map { #","pr_id":\#($0)"# } ?? ""
        return try decode(#"{"id":\#(id),"session_id":"s1","title":"Q\#(id)","options":[],"links":[],"attachments":[],"origin":"ask","status":"open","asked_at":\#(id)\#(link)}"#)
    }

    private static func report(_ id: UInt64, pr: UInt64?) throws -> Report {
        let link = pr.map { #","pr_id":\#($0)"# } ?? ""
        return try decode(#"{"id":\#(id),"session_id":"s1","title":"R\#(id)","links":[],"attachments":[],"kind":"notice","created_at":\#(id)\#(link)}"#)
    }

    private static func pullRequest(_ id: UInt64) throws -> PullRequest {
        try decode(#"{"id":\#(id),"url":"https://github.com/a/b/pull/\#(id)","repo":"a/b","number":\#(id),"session_id":"s1","title":"PR","state":"open","opened_at":1}"#)
    }

    struct Case: Sendable, CustomTestStringConvertible {
        let name: String
        let prId: UInt64?
        let cards: [UInt64]
        let inInbox: Bool
        var testDescription: String { name }
    }

    @Test(arguments: [
        Case(name: "a question about a waiting PR goes inside its card", prId: 4, cards: [4], inInbox: false),
        Case(name: "a question whose card is gone is a card of its own", prId: 4, cards: [7], inInbox: true),
        Case(name: "a question with no PR is a card of its own", prId: nil, cards: [4], inInbox: true),
    ])
    func questionsGoInsideTheirWaitingPullRequest(_ c: Case) throws {
        let grouping = InboxGrouping(questions: [try Self.question(1, pr: c.prId)], pullRequests: try c.cards.map(Self.pullRequest))
        #expect(grouping.loose.map(\.id) == (c.inInbox ? [1] : []))
        #expect(grouping.byPullRequest.values.flatMap(\.self).map(\.id) == (c.inInbox ? [] : [1]))
        if !c.inInbox, let pr = c.prId {
            #expect(grouping.byPullRequest[pr]?.map(\.id) == [1])
        }
    }

    @Test(arguments: [
        Case(name: "a report about a waiting PR goes inside its card", prId: 4, cards: [4], inInbox: false),
        Case(name: "a report whose card is gone is a card of its own", prId: 4, cards: [7], inInbox: true),
        Case(name: "a report with no PR is a card of its own", prId: nil, cards: [4], inInbox: true),
    ])
    func reportsGoInsideTheirWaitingPullRequest(_ c: Case) throws {
        let grouping = InboxGrouping(questions: [], reports: [try Self.report(1, pr: c.prId)], pullRequests: try c.cards.map(Self.pullRequest))
        #expect(grouping.looseReports.map(\.id) == (c.inInbox ? [1] : []))
        #expect(grouping.reportsByPullRequest.values.flatMap(\.self).map(\.id) == (c.inInbox ? [] : [1]))
        if !c.inInbox, let pr = c.prId {
            #expect(grouping.reportsByPullRequest[pr]?.map(\.id) == [1])
        }
    }
}
