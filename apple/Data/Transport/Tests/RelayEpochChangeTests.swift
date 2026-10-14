@testable import AlondraTransport
import CryptoKit
import Foundation
import Testing

/// A relay that, like the real one after a restart, refuses a request signed for its previous
/// epoch with a 401 that names the change and the epoch it uses now.
private actor RestartingRelay: HTTPTransport {
    private var epoch = Data(repeating: 7, count: 16)
    private(set) var bootstraps = 0
    private(set) var signedEpochs: [Data] = []
    func restart() { epoch = Data(repeating: 8, count: 16) }
    func send(_ request: URLRequest) async throws -> HTTPReply {
        let url = try #require(request.url)
        if url.path == "/v1/bootstrap" {
            bootstraps += 1
            return HTTPReply(status: 200, body: Data("{\"protocol\":1,\"epoch\":\"\(Base64URL.encode(epoch))\"}".utf8))
        }
        let signed = try Base64URL.decode(#require(request.value(forHTTPHeaderField: "x-relay-epoch")))
        signedEpochs.append(signed)
        guard signed == epoch else {
            return HTTPReply(status: 401, body: Data("{\"error\":\"relay_epoch_changed\",\"epoch\":\"\(Base64URL.encode(epoch))\"}".utf8))
        }
        return HTTPReply(status: 200, body: Data("{\"request_id\":\"\(url.lastPathComponent)\",\"status\":\"completed\",\"revision\":1}".utf8))
    }
}

private actor Refusals {
    private(set) var count = 0
    func add() { count += 1 }
}

@Suite struct RelayEpochChangeTests {
    private func setup() throws -> (any Transport, RestartingRelay, Refusals) {
        let signing = try P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 3, count: 32))
        let relay = RestartingRelay()
        let refusals = Refusals()
        let transport = try AlondraAPIClient.makeTransport(base: URL(string: "https://relay.fixture.invalid")!, deviceID: "phone-1",
            signer: { try signing.signature(for: $0).rawRepresentation }, bearer: { "token" },
            bearerRefused: { await refusals.add() }, transport: relay, now: { 1_000 })
        return (transport, relay, refusals)
    }

    @Test func aRequestSignedBeforeARelayRestartIsSignedAgainInsteadOfFailing() async throws {
        let (transport, relay, refusals) = try setup()
        _ = try await transport.status(requestID: "before")
        await relay.restart()
        let job = try await transport.status(requestID: "after")
        #expect(job.requestID == "after")
        let old = Data(repeating: 7, count: 16), new = Data(repeating: 8, count: 16)
        #expect(await relay.signedEpochs == [old, old, new])
        #expect(await relay.bootstraps == 2)
        #expect(await refusals.count == 0, "a stale epoch is not a refused access token")
    }

    @Test func aRestartBeforeTheFirstSignedRequestDoesNotRenewTheAccessToken() async throws {
        let (transport, relay, refusals) = try setup()
        _ = try await transport.bootstrap()
        await relay.restart()
        let job = try await transport.status(requestID: "first")
        #expect(job.requestID == "first")
        #expect(await relay.bootstraps == 2)
        #expect(await refusals.count == 0, "a stale epoch is not a refused access token")
    }
}
