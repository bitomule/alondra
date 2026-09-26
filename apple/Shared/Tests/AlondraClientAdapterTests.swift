import AlondraAPIClient
import AlondraShared
import Foundation
import Testing

private actor StubTransport: HTTPTransport {
    private var responses: [HTTPResponse]
    private(set) var paths: [String] = []

    init(_ responses: [HTTPResponse]) { self.responses = responses }

    func send(_ request: URLRequest, policy: RequestPolicy) async throws -> HTTPResponse {
        paths.append(request.url!.relativeString)
        return responses.removeFirst()
    }

    func upload(_ request: URLRequest, body: Data, progress: @escaping @Sendable (Double) -> Void) async throws -> HTTPResponse {
        Issue.record("Unexpected upload")
        return HTTPResponse(status: 500, body: Data())
    }
}

@Suite struct AlondraClientAdapterTests {
    @Test func emptyForbiddenAndConflictKeepTheirActualStatus() {
        #expect(AlondraError.from(.forbidden(kind: nil, detail: nil, reasons: [])) == .unexpected(403))
        #expect(AlondraError.from(.conflict(kind: nil, detail: nil, reasons: [])) == .unexpected(409))
    }

    @Test func missingActivityRouteFallsBackToTheNodeButAnAlondrad404DoesNot() async throws {
        let detail = Data(#"{"node":{"session_id":"s1","activity":[{"agent_id":"a1","agent_type":"tester","tool":"mav","since":6}]},"timeline":[]}"#.utf8)
        let older = StubTransport([HTTPResponse(status: 404, body: Data()), HTTPResponse(status: 200, body: detail)])
        let client = AlondraClient(base: URL(string: "http://127.0.0.1:47800")!, transport: older)
        let threads = try await client.nodeThreads("s1")
        #expect(threads.activity?.map(\.thread) == ["tester"])
        #expect(await older.paths == ["http://127.0.0.1:47800/v1/nodes/s1/activity", "http://127.0.0.1:47800/v1/nodes/s1?limit=1"])

        let gone = StubTransport([HTTPResponse(status: 404, body: Data(#"{"error":"gone"}"#.utf8))])
        let goneClient = AlondraClient(base: URL(string: "http://127.0.0.1:47800")!, transport: gone)
        await #expect(throws: AlondraError.server("Alondra no longer has it")) {
            try await goneClient.nodeThreads("s1")
        }
        #expect(await gone.paths.count == 1)
    }
}
