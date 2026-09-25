import AlondraAPIClient
import AlondraShared
import Foundation
import Testing

private actor DeferredUnauthorizedTransport: HTTPTransport {
    let started: AsyncStream<Void>
    private let startedContinuation: AsyncStream<Void>.Continuation
    private var response: CheckedContinuation<HTTPResponse, any Error>?

    init() {
        let pair = AsyncStream<Void>.makeStream()
        started = pair.stream
        startedContinuation = pair.continuation
    }

    func send(_ request: URLRequest, policy: RequestPolicy) async throws -> HTTPResponse {
        try await withCheckedThrowingContinuation { continuation in
            response = continuation
            startedContinuation.yield()
        }
    }

    func upload(_ request: URLRequest, body: Data, progress: @escaping @Sendable (Double) -> Void) async throws -> HTTPResponse {
        throw URLError(.unsupportedURL)
    }

    func reject() {
        response?.resume(returning: HTTPResponse(status: 401, body: Data()))
        response = nil
    }
}

@MainActor
@Suite struct FleetRefreshIdentityTests {
    @Test func lateUnauthorizedResponseCannotMarkNewServerUnpaired() async throws {
        let transport = DeferredUnauthorizedTransport()
        let old = AlondraClient(base: URL(string: "http://127.0.0.1:47800")!, transport: transport)
        let new = AlondraClient(base: URL(string: "http://127.0.0.1:47801")!)
        let fleet = FleetStore(client: old)

        let refresh = Task { await fleet.refresh(ifCurrentClientMatches: old.identity) }
        var started = transport.started.makeAsyncIterator()
        _ = await started.next()
        fleet.use(new)
        await transport.reject()
        await refresh.value

        #expect(fleet.client.identity == new.identity)
        #expect(fleet.connection == .connecting)
        #expect(fleet.overview?.tick == nil)
    }
}
