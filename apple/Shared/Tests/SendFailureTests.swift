import AlondraShared
import Foundation
import Testing

@Suite struct SendFailureTests {
    private static let cloudflare = """
    <!DOCTYPE html>
    <!--[if lt IE 7]> <html class="no-js ie6 oldie" lang="en-US"> <![endif]-->
    <head><title>davidcollado.dev | 502: Bad gateway</title></head>
    """

    @Test(arguments: [
        (502, cloudflare, AlondraError.unreachable),
        (503, "", AlondraError.unreachable),
        (504, #"{"error":"upstream timed out"}"#, AlondraError.unreachable),
        (500, cloudflare, AlondraError.unexpected(500)),
        (404, "", AlondraError.unexpected(404)),
        (422, "not json at all", AlondraError.unexpected(422)),
        (409, #"{"error":"question #7 is no longer open"}"#, AlondraError.server("question #7 is no longer open")),
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
    private actor Attempts {
        var count = 0
        func next() -> Int {
            count += 1
            return count
        }
    }

    private func question() throws -> InboxItem {
        let json = #"{"id":7,"session_id":"s","title":"t","options":[],"links":[],"attachments":[],"origin":"ask","status":"open","asked_at":1}"#
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return .question(try decoder.decode(Question.self, from: Data(json.utf8)))
    }

    private func settle(_ fleet: FleetStore) async {
        for _ in 0..<200 where !fleet.pendingSends.isEmpty {
            try? await Task.sleep(for: .milliseconds(5))
        }
    }

    @Test func anAnswerSentWhileAlondraIsDownIsKeptAndSentWhenItIsBack() async throws {
        let fleet = FleetStore(retryDelay: .milliseconds(1))
        let attempts = Attempts()
        let item = try question()
        let ok = await fleet.resolve(item) { _ in
            if await attempts.next() < 3 { throw AlondraError.unreachable }
        }
        #expect(ok, "the card leaves; the answer is not lost")
        #expect(fleet.pendingSends.count == 1)
        #expect(fleet.notice == nil, "no error for something that will be retried")
        await settle(fleet)
        #expect(fleet.pendingSends.isEmpty)
        #expect(await attempts.count == 3)
        #expect(fleet.isHidden(item.id))
    }

    @Test func aRealRefusalDuringRetriesBringsTheCardBackWithAShortMessage() async throws {
        let fleet = FleetStore(retryDelay: .milliseconds(1))
        let attempts = Attempts()
        let item = try question()
        _ = await fleet.resolve(item) { _ in
            if await attempts.next() == 1 { throw URLError(.timedOut) }
            throw AlondraError.server("question #7 is no longer open")
        }
        await settle(fleet)
        #expect(fleet.pendingSends.isEmpty)
        #expect(!fleet.isHidden(item.id))
        #expect(fleet.notice?.failure == true)
        #expect(fleet.notice?.text.contains("no longer open") == true)
    }
}
