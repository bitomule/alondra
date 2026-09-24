import AlondraShared
import Foundation
import Testing

@Suite struct SendFailureTests {
    private static let cloudflare = """
    <!DOCTYPE html>
    <!--[if lt IE 7]> <html class="no-js ie6 oldie" lang="en-US"> <![endif]-->
    <head><title>davidcollado.dev | 502: Bad gateway</title></head>
    """

    /// alondra/retry-queue, 24 Sep: gh refusing to merge the conflicting PR #54 came back as
    /// alondrad's own 502, which counted as "Alondra is down" and was retried forever.
    @Test(arguments: [
        (502, cloudflare, AlondraError.unreachable),
        (503, "", AlondraError.unreachable),
        (504, "<html>gateway timeout</html>", AlondraError.unreachable),
        (502, #"{"error":"X Pull request a/b#54 is not mergeable"}"#, AlondraError.server("X Pull request a/b#54 is not mergeable")),
        (502, #"{"error":"X Pull request a/b#54 is not mergeable","kind":"merge_refused"}"#, AlondraError.server("GitHub didn’t merge it: X Pull request a/b#54 is not mergeable")),
        (504, #"{"error":"gh pr merge did not finish within 90 s"}"#, AlondraError.server("gh pr merge did not finish within 90 s")),
        (422, #"{"error":"X Pull request a/b#54 is not mergeable","kind":"merge_refused"}"#, AlondraError.server("GitHub didn’t merge it: X Pull request a/b#54 is not mergeable")),
        (409, #"{"error":"https://github.com/a/b/pull/54 is no longer open","kind":"pr_not_open"}"#, AlondraError.server("This PR is no longer open")),
        (409, #"{"error":"question #7 is no longer open","kind":"question_closed"}"#, AlondraError.server("This question was already answered")),
        (409, #"{"error":"question #7 is no longer open"}"#, AlondraError.server("question #7 is no longer open")),
        (409, #"{"error":"the task is already done","kind":"task_refused"}"#, AlondraError.server("The task’s state does not allow that now")),
        (404, #"{"error":"task t-9 does not exist"}"#, AlondraError.server("Alondra no longer has it")),
        (500, cloudflare, AlondraError.unexpected(500)),
        (404, "", AlondraError.unexpected(404)),
        (422, "not json at all", AlondraError.unexpected(422)),
        (401, cloudflare, AlondraError.unauthorized),
    ])
    func onlyAlondrasOwnErrorsReachTheUserAsText(status: Int, body: String, expected: AlondraError) {
        let error = AlondraError.from(status: status, body: Data(body.utf8))
        #expect(error == expected)
        #expect(!(error.errorDescription ?? "").contains("<"), "no markup is ever shown")
    }

    @Test func aLongServerMessageIsCut() {
        let long = String(repeating: "x", count: 5000)
        let error = AlondraError.from(status: 409, body: Data(#"{"error":"\#(long)"}"#.utf8))
        #expect((error.errorDescription ?? "").count <= AlondraError.longestMessage + 1)
    }

    @Test(arguments: [
        (AlondraError.unreachable as any Error, true),
        (URLError(.timedOut), true),
        (URLError(.notConnectedToInternet), true),
        (URLError(.cannotConnectToHost), true),
        (AlondraError.from(status: 502, body: Data(#"{"error":"not mergeable","kind":"merge_refused"}"#.utf8)), false),
        (AlondraError.server("question #7 is no longer open"), false),
        (AlondraError.unexpected(500), false),
        (AlondraError.unauthorized, false),
        (URLError(.cancelled), false),
    ])
    func onlyFailuresThatSayNothingAboutTheRequestAreRetried(error: any Error, transient: Bool) {
        #expect(error.isTransient == transient)
    }

    @Test func retriesBackOffUpToAMinute() {
        let waits = (0..<8).map { FleetStore.retryWait(attempt: $0, base: .seconds(2)) }
        #expect(waits.prefix(4) == [.seconds(2), .seconds(4), .seconds(8), .seconds(16)])
        #expect(waits.last == .seconds(60))
    }
}

@MainActor
@Suite struct SendQueueTests {
    /// What each attempt gets back, in order; the last one repeats.
    private actor Script {
        let outcomes: [(any Error)?]
        var count = 0

        init(_ outcomes: [(any Error)?]) {
            self.outcomes = outcomes
        }

        func next() throws {
            let outcome = outcomes[min(count, outcomes.count - 1)]
            count += 1
            if let outcome { throw outcome }
        }
    }

    private static func refusal(_ status: Int, _ json: String) -> any Error {
        AlondraError.from(status: status, body: Data(json.utf8))
    }

    private static let overviewJSON = #"{"tick":1,"nodes":[],"open_questions":[],"news":[],"orchestrator_messages":[],"machine":{"memory_total":1,"memory_available":1,"load":[],"disk_available":1,"unreliable":false},"prs":[{"id":54,"url":"https://github.com/a/b/pull/54","repo":"a/b","number":54,"session_id":"s","title":"t","opened_at":1,"state":"open","additions":0,"deletions":0,"changed_files":0,"checks":[],"merge_methods":["merge"]}]}"#

    private func decoder() -> JSONDecoder {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return decoder
    }

    private func overview() throws -> Overview {
        try decoder().decode(Overview.self, from: Data(Self.overviewJSON.utf8))
    }

    private func pullRequest() throws -> InboxItem {
        .pullRequest(try #require(try overview().prs?.first))
    }

    private func settle(_ fleet: FleetStore) async {
        for _ in 0..<400 where !fleet.pendingSends.isEmpty {
            try? await Task.sleep(for: .milliseconds(5))
        }
    }

    struct Case: Sendable, CustomTestStringConvertible {
        let name: String
        let outcomes: [(any Error)?]
        let cardBack: Bool
        let notice: String?
        let stuck: Int
        var testDescription: String { name }
    }

    static let cases: [Case] = [
        Case(name: "back after two failures", outcomes: [AlondraError.unreachable, URLError(.timedOut), nil], cardBack: false, notice: nil, stuck: 0),
        Case(name: "already merged, alondrad says so with a 200", outcomes: [URLError(.timedOut), nil], cardBack: false, notice: nil, stuck: 0),
        Case(name: "gh refuses, alondrad's own 502", outcomes: [URLError(.timedOut), refusal(502, #"{"error":"not mergeable","kind":"merge_refused"}"#)], cardBack: true, notice: "GitHub didn’t merge it: not mergeable", stuck: 0),
        Case(name: "no longer open", outcomes: [URLError(.timedOut), refusal(409, #"{"error":"pull/54 is no longer open","kind":"pr_not_open"}"#)], cardBack: true, notice: "This PR is no longer open", stuck: 0),
        Case(name: "never answers", outcomes: [AlondraError.unreachable], cardBack: true, notice: nil, stuck: 1),
    ]

    @Test(arguments: cases)
    func aPendingSendEndsWithWhatTheServerSaid(_ c: Case) async throws {
        let fleet = FleetStore(retryDelay: .milliseconds(1), stuckAfter: .milliseconds(150))
        let script = Script(c.outcomes)
        let item = try pullRequest()
        let ok = await fleet.resolve(item) { _ in try await script.next() }
        #expect(ok, "the card leaves; what was sent is not lost")
        #expect(fleet.pendingSends.count == 1)
        await settle(fleet)
        #expect(fleet.pendingSends.isEmpty)
        #expect(fleet.isHidden(item.id) == !c.cardBack)
        #expect(fleet.notice?.text == c.notice.map { "Couldn’t send: \($0)" })
        #expect(fleet.notice?.failure ?? true)
        #expect(fleet.stuckSends.count == c.stuck)
        if c.stuck > 0 {
            fleet.dismissStuckSends()
            #expect(fleet.stuckSends.isEmpty)
        }
    }

    enum Wake: String, CaseIterable, Sendable {
        case foreground, reconnect
    }

    @Test(arguments: Wake.allCases)
    func aPendingSendGoesOutAtOnceWhenTheAppCanReachAlondraAgain(_ wake: Wake) async throws {
        let fleet = FleetStore(retryDelay: .seconds(30), stuckAfter: .seconds(600))
        let script = Script([AlondraError.unreachable, nil])
        let item = try pullRequest()
        _ = await fleet.resolve(item) { _ in try await script.next() }
        try await Task.sleep(for: .milliseconds(50))
        #expect(fleet.pendingSends.count == 1, "the backoff alone would wait 30 s")
        switch wake {
        case .foreground: fleet.retryPendingSends()
        case .reconnect: fleet.apply(try overview())
        }
        await settle(fleet)
        #expect(fleet.pendingSends.isEmpty)
        #expect(await script.count == 2)
    }

    @Test func aPendingCardStaysOutOfTheInboxWhileTheOverviewStillListsIt() async throws {
        let fleet = FleetStore(retryDelay: .seconds(30), stuckAfter: .seconds(600))
        let item = try pullRequest()
        fleet.apply(try overview())
        _ = await fleet.resolve(item) { _ in throw AlondraError.unreachable }
        fleet.apply(try overview())
        #expect(fleet.isHidden(item.id))
        #expect(!fleet.inbox.contains { $0.id == item.id })
    }

    /// pr-card-ask review, 24 Sep: asking about a PR never parks its card, so sending it through
    /// `resolve` (which hides the card until the next full reload) undid the fix. The one Ask every
    /// screen uses must leave a still-live card exactly where it was, even while it waits to go out.
    @Test func askingAboutAPullRequestNeverHidesItsCard() async throws {
        let nowhere = AlondraClient(base: try #require(URL(string: "http://127.0.0.1:9")))
        let fleet = FleetStore(client: nowhere, retryDelay: .seconds(30), stuckAfter: .seconds(600))
        let item = try pullRequest()
        fleet.apply(try overview())
        guard case .pullRequest(let pr) = item else { return }
        let ok = await fleet.ask(onPullRequest: pr.id, text: "Does it build?")
        #expect(ok, "kept to send again, not lost")
        #expect(!fleet.isHidden(item.id))
        #expect(fleet.inbox.contains { $0.id == item.id })
    }
}
