import AlondraContracts
import AlondraDomain
import Foundation
import Testing

@Suite struct ReportActionTests {
    private func report(kind: String) throws -> Report {
        let json = """
        {"id":1,"sessionId":"s1","title":"Done","links":[],"attachments":[],"kind":"\(kind)","createdAt":1}
        """
        return try JSONDecoder().decode(Report.self, from: Data(json.utf8))
    }

    @Test(arguments: [(#","looks_like_question":true"#, true), (#","looks_like_question":false"#, false), ("", false)])
    func aReportTheGateFlaggedSaysSo(field: String, flagged: Bool) throws {
        let json = #"{"id":1,"session_id":"s1","title":"t","links":[],"attachments":[],"kind":"notice","created_at":1\#(field)}"#
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        #expect(try decoder.decode(Report.self, from: Data(json.utf8)).seemsToAsk == flagged)
    }

    /// 24 Sep: David comments on an artifact in the browser; its card hands those comments to the node.
    @Test(arguments: ["final", "document"])
    func aReportThatSendsAnArtifactIsReviewed(kind: String) throws {
        let json = """
        {"id":1,"sessionId":"s1","title":"Plan","links":[{"url":"https://claude.ai/artifact/plan"}],"attachments":[],"kind":"\(kind)","createdAt":1}
        """
        let report = try JSONDecoder().decode(Report.self, from: Data(json.utf8))
        #expect(report.primaryAction() == .review)
    }

    @Test func markingAsReadIsTheDefaultOtherwise() throws {
        for kind in ["final", "notice", "document", "progress"] {
            #expect(try report(kind: kind).primaryAction() == .markRead)
        }
    }
}
