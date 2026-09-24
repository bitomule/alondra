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

    /// 24 Sep: David's «Leer y cerrar» has to leave the report read even when alondrad refuses
    /// the close; the refusal alert then says what to do about the node.
    @Test(arguments: [
        (nil, ["close", "read"], nil),
        (CloseRefused(message: "está trabajando; espera a que termine") as (any Error)?, ["close", "read"], "está trabajando; espera a que termine"),
        (URLError(.notConnectedToInternet) as (any Error)?, ["close"], nil),
    ])
    func readAndCloseReadsTheReportUnlessTheCloseNeverReachedAlondrad(failure: (any Error)?, calls: [String], refusal: String?) async {
        let log = CallLog()
        var thrown: (any Error)?
        do {
            try await AlondraClient.readAndClose(
                close: {
                    await log.add("close")
                    if let failure { throw failure }
                },
                markRead: { await log.add("read") }
            )
        } catch {
            thrown = error
        }
        #expect(await log.calls == calls)
        #expect((thrown as? CloseRefused)?.message == refusal)
        #expect((thrown == nil) == (failure == nil))
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

    /// 24 Sep: David comments on an artifact in the browser; its card hands those comments to the node.
    @Test(arguments: ["final", "document"])
    func aReportThatSendsAnArtifactIsReviewed(kind: String) throws {
        let json = """
        {"id":1,"sessionId":"s1","title":"Plan","links":[{"url":"https://claude.ai/artifact/plan"}],"attachments":[],"kind":"\(kind)","createdAt":1}
        """
        let report = try JSONDecoder().decode(Report.self, from: Data(json.utf8))
        #expect(report.primaryAction(nodeFound: true) == .review)
    }

    @Test func markingAsReadStaysFirstOtherwise() throws {
        #expect(try report(kind: "final").primaryAction(nodeFound: false) == .markRead, "nothing to close")
        for kind in ["notice", "document", "progress"] {
            #expect(try report(kind: kind).primaryAction(nodeFound: true) == .markRead)
        }
    }
}

private actor CallLog {
    var calls: [String] = []

    func add(_ call: String) {
        calls.append(call)
    }
}
