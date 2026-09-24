import AlondraShared
import Foundation
import Testing

@Suite struct ReportActionTests {
    private func report(kind: String) throws -> Report {
        let json = """
        {"id":1,"sessionId":"s1","title":"Done","links":[],"attachments":[],"kind":"\(kind)","createdAt":1}
        """
        return try JSONDecoder().decode(Report.self, from: Data(json.utf8))
    }

    @Test func aFinishedNodeIsClosedByDefault() throws {
        #expect(try report(kind: "final").primaryAction(nodeFound: true) == .readAndClose)
    }

    @Test(arguments: [(#","looks_like_question":true"#, true), (#","looks_like_question":false"#, false), ("", false)])
    func aReportTheGateFlaggedSaysSo(field: String, flagged: Bool) throws {
        let json = #"{"id":1,"session_id":"s1","title":"t","links":[],"attachments":[],"kind":"notice","created_at":1\#(field)}"#
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        #expect(try decoder.decode(Report.self, from: Data(json.utf8)).seemsToAsk == flagged)
    }

    private func node(orchestrator: Bool) throws -> Node {
        let json = """
        {"sessionId":"s1","shortId":"s1","alive":true,"isOrchestrator":\(orchestrator),"hasPlugin":true,"state":"done","stateSince":1,"stateSource":"hook"}
        """
        return try JSONDecoder().decode(Node.self, from: Data(json.utf8))
    }

    /// 24 Sep: David keeps the orchestrator alive on purpose, so its cards never offer to close it.
    @Test(arguments: [(false, true), (true, false)])
    func onlyANodeThatIsNotTheOrchestratorCanBeClosedFromItsCard(orchestrator: Bool, offered: Bool) throws {
        let final = try report(kind: "final")
        #expect((final.closableNode(try node(orchestrator: orchestrator)) != nil) == offered)
        #expect(final.closableNode(nil) == nil)
    }

    @Test func markingAsReadStaysFirstOtherwise() throws {
        #expect(try report(kind: "final").primaryAction(nodeFound: false) == .markRead, "nothing to close")
        for kind in ["notice", "document", "progress"] {
            #expect(try report(kind: kind).primaryAction(nodeFound: true) == .markRead)
        }
    }
}
