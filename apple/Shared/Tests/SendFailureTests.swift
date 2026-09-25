import AlondraShared
import AlondraAPIClient
import AlondraCommands
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

}

@MainActor
@Suite struct CommandFailureTests {
    private actor ReplyTransport: HTTPTransport {
        var paths: [String] = []
        func send(_ request: URLRequest, policy: RequestPolicy) async throws -> HTTPResponse {
            let path = request.url?.path() ?? ""
            paths.append(path)
            if path.hasSuffix("/read") { throw URLError(.timedOut) }
            return HTTPResponse(status: 200, body: Data("{}".utf8))
        }
        func upload(_ request: URLRequest, body: Data, progress: @escaping @Sendable (Double) -> Void) async throws -> HTTPResponse {
            Issue.record("Reply must not upload")
            return HTTPResponse(status: 500, body: Data())
        }
    }

    private struct TestSigner: RequestSigner {
        let deviceID = "phone"
        let signatureByte: UInt8
        func signature(for message: Data) throws -> Data { Data([signatureByte]) }
    }

    private func overview() throws -> Overview {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(Overview.self, from: Data(#"{"tick":1,"nodes":[],"open_questions":[],"news":[],"orchestrator_messages":[],"machine":{"memory_total":1,"memory_available":1,"load":[0],"disk_available":1,"unreliable":false}}"#.utf8))
    }

    private actor Counter {
        var calls = 0
        func sent() { calls += 1 }
    }

    @Test func anUnansweredMutationIsNeverRetriedOnReconnect() async throws {
        let client = AlondraClient(base: URL(string: "http://127.0.0.1:47800")!)
        let fleet = FleetStore(client: client)
        let counter = Counter()
        let accepted = await fleet.perform(.send("node-1")) { _ in
            await counter.sent()
            throw URLError(.timedOut)
        }
        #expect(!accepted, "unknown is not a confirmed send")
        #expect(fleet.uncertainSends.count == 1)
        #expect(await counter.calls == 1)
        fleet.apply(try overview())
        #expect(fleet.connection == .online)
        #expect(await counter.calls == 1, "reconnect must not dispatch the mutation again")
        await fleet.dismissUncertainSends()
        #expect(fleet.uncertainSends.isEmpty)
        #expect(await counter.calls == 1)
    }

    @Test func relaunchedFleetRestoresWarningAndBlocksDuplicateSend() async throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("fleet-command-journal-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let client = AlondraClient(base: URL(string: "https://example.test")!)
        let scope = CommandScope(server: client.base, deviceID: client.device,
            credentialRevision: "test-key", generation: 1)
        let first = FleetStore(client: client,
            commandGate: CommandGate(scope: scope, journal: FileCommandJournal(directory: directory)),
            commandScope: scope)
        let sent = await first.perform(.newRequest) { _ in throw URLError(.timedOut) }
        #expect(!sent)
        #expect(first.uncertainSends.count == 1)

        let relaunchedScope = CommandScope(server: client.base, deviceID: client.device,
            credentialRevision: "test-key", generation: 1)
        let relaunched = FleetStore(client: client,
            commandGate: CommandGate(scope: relaunchedScope, journal: FileCommandJournal(directory: directory)),
            commandScope: relaunchedScope)
        await relaunched.restoreUncertainSends()
        #expect(relaunched.uncertainSends.map(\.id) == first.uncertainSends.map(\.id))
        let duplicate = await relaunched.perform(.newRequest) { _ in
            Issue.record("An unresolved command was sent again after app restart")
        }
        #expect(!duplicate)
        await relaunched.dismissUncertainSends()
        #expect(relaunched.uncertainSends.isEmpty)
    }

    @Test func anUnansweredCommandCannotBeSentToAnotherServer() async throws {
        let a = AlondraClient(base: URL(string: "http://a.example")!)
        let b = AlondraClient(base: URL(string: "http://b.example")!)
        let fleet = FleetStore(client: a)
        let counter = Counter()
        _ = await fleet.perform(.newRequest) { _ in
            await counter.sent()
            throw URLError(.timedOut)
        }
        #expect(fleet.uncertainSends.count == 1)
        fleet.use(b)
        #expect(fleet.uncertainSends.isEmpty)
        #expect(await counter.calls == 1)
    }

    @Test func rotatingCredentialAtSameAddressClearsPresentationAndUncertainCommands() async throws {
        let base = URL(string: "http://a.example")!
        let client = AlondraClient(base: base, signer: TestSigner(signatureByte: 1))
        let newClient = AlondraClient(base: base, signer: TestSigner(signatureByte: 2))
        let fleet = FleetStore(client: client)
        _ = await fleet.perform(.newRequest) { _ in throw URLError(.timedOut) }
        #expect(fleet.uncertainSends.count == 1)
        fleet.setConnection(.online)
        fleet.resetPresentationForIdentityChange()
        fleet.use(newClient)
        #expect(fleet.uncertainSends.isEmpty)
        #expect(fleet.connection == .connecting)
        #expect(fleet.overview == nil)
        #expect(try fleet.client.signer?.signature(for: Data()) == Data([2]))
    }

    @Test func replyWithLostReadResponseDoesNotRepeatMessageOnReconnect() async throws {
        let transport = ReplyTransport()
        let client = AlondraClient(base: URL(string: "https://a.example")!, transport: transport)
        let fleet = FleetStore(client: client)
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let report = try decoder.decode(Report.self, from: Data(#"{"id":7,"session_id":"node-1","title":"Review","links":[],"attachments":[],"kind":"final","created_at":1}"#.utf8))
        let confirmed = await fleet.reply(to: report, text: "Please fix this")
        #expect(!confirmed)
        #expect(fleet.uncertainSends.map(\.kind) == [.replyToReport(7)])
        fleet.apply(try overview())
        #expect(await transport.paths == ["/v1/nodes/node-1/messages", "/v1/reports/7/read"])
    }

    @Test func oldInFlightResponseCannotModifyNewServerPresentation() async throws {
        let a = AlondraClient(base: URL(string: "https://a.example")!)
        let b = AlondraClient(base: URL(string: "https://b.example")!)
        let fleet = FleetStore(client: a)
        let entered = AsyncStream.makeStream(of: Void.self)
        let release = AsyncStream.makeStream(of: Void.self)
        let attempt = Task {
            await fleet.perform(.newRequest) { _ in
                entered.continuation.yield(())
                for await _ in release.stream { break }
                throw URLError(.timedOut)
            }
        }
        for await _ in entered.stream { break }
        fleet.use(b)
        release.continuation.yield(())
        #expect(await attempt.value == false)
        #expect(fleet.uncertainSends.isEmpty)
        #expect(fleet.client.base == b.base)
        #expect(fleet.connection == .connecting)
    }

    @Test func delayedGestureAndHandoffFromOldServerCannotExecuteOnNewServer() async throws {
        let a = AlondraClient(base: URL(string: "https://a.example")!)
        let b = AlondraClient(base: URL(string: "https://b.example")!)
        let fleet = FleetStore(client: a)
        let tokenA = fleet.presentationToken
        let counter = Counter()
        let question = try JSONDecoder().decode(Question.self, from: Data(#"{"id":42,"sessionId":"node","title":"Q","options":[],"links":[],"attachments":[],"origin":"ask","status":"open","askedAt":1}"#.utf8))
        let handoff = FleetStore.Handoff(presentationToken: tokenA, itemID: "q42", label: "old decision", kind: .answer(42)) { _ in
            await counter.sent()
        }

        fleet.use(b)
        fleet.publishHandoff(handoff)
        #expect(fleet.takeHandoff(for: "q42") == nil)
        let accepted = await fleet.resolve(.question(question), kind: .answer(42), expectedPresentationToken: tokenA) { _ in
            await counter.sent()
        }
        #expect(!accepted)
        #expect(await counter.calls == 0)
    }
}
