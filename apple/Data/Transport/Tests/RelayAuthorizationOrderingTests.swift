@testable import AlondraTransport
import CryptoKit
import Foundation
import Testing

private actor RelayCredentials {
    private var token = "old-token"
    private(set) var refusals = 0
    func value() -> String { token }
    func replace(_ value: String) { token = value }
    func refuse() { refusals += 1; token = "refused-token" }
    func refuse(_ rejected: String) { if token == rejected { refuse() } }
}

private actor AuthorizationOrderingPeer: HTTPTransport {
    let entered: AsyncStream<Void>
    private let arrival: AsyncStream<Void>.Continuation
    private let signing: P256.Signing.PublicKey
    private var held: CheckedContinuation<HTTPReply, Never>?
    private var currentEpoch = Data(repeating: 7, count: 16)
    private var requiredToken = "old-token"
    private var rejectBootstrap = false
    private var refuseNext = false
    init(signing: P256.Signing.PublicKey) {
        self.signing = signing
        (entered, arrival) = AsyncStream.makeStream(bufferingPolicy: .bufferingNewest(1))
    }
    func changeEpoch() { currentEpoch = Data(repeating: 8, count: 16) }
    func requireToken(_ token: String) { requiredToken = token }
    func prohibitBootstrap() { rejectBootstrap = true }
    func rejectNextStatus() { refuseNext = true }
    func release(status: Int) {
        held?.resume(returning: reply(id: "held-A", status: status)); held = nil
    }
    private func reply(id: String, status: Int = 200) -> HTTPReply {
        HTTPReply(status: status, body: Data("{\"request_id\":\"\(id)\",\"status\":\"completed\",\"revision\":1}".utf8))
    }
    func send(_ request: URLRequest) async throws -> HTTPReply {
        let url = try #require(request.url)
        if url.path == "/v1/bootstrap" {
            guard !rejectBootstrap else { throw URLError(.cannotConnectToHost) }
            return HTTPReply(status: 200, body: Data("{\"protocol\":1,\"epoch\":\"\(Base64URL.encode(currentEpoch))\"}".utf8))
        }
        let epoch = try Base64URL.decode(#require(request.value(forHTTPHeaderField: "x-relay-epoch")))
        let nonce = try Base64URL.decode(#require(request.value(forHTTPHeaderField: "x-device-nonce")))
        let timestampText = try #require(request.value(forHTTPHeaderField: "x-device-timestamp-ms"))
        let timestamp = try #require(UInt64(timestampText))
        let target = url.path + (url.query.map { "?" + $0 } ?? "")
        let challenge = try RelayHTTPChallenge.message(deviceID: "phone-1", method: "GET", target: target,
            epoch: epoch, body: Data(), timestampMilliseconds: timestamp, nonce: nonce)
        let signature = try P256.Signing.ECDSASignature(rawRepresentation: Base64URL.decode(#require(request.value(forHTTPHeaderField: "x-device-signature"))))
        #expect(signing.isValidSignature(signature, for: challenge))
        if url.lastPathComponent == "held-A" {
            return await withCheckedContinuation { continuation in held = continuation; arrival.yield(()) }
        }
        if refuseNext { refuseNext = false; return reply(id: url.lastPathComponent, status: 401) }
        guard epoch == currentEpoch, request.value(forHTTPHeaderField: "Authorization") == "Bearer " + requiredToken else {
            return reply(id: url.lastPathComponent, status: 401)
        }
        return reply(id: url.lastPathComponent)
    }
}

@Suite struct RelayAuthorizationOrderingTests {
    private func setup(conditionalRefusal: Bool = false) throws -> (any Transport, AuthorizationOrderingPeer, RelayCredentials) {
        let signing = try P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 3, count: 32))
        let peer = AuthorizationOrderingPeer(signing: signing.publicKey)
        let credentials = RelayCredentials()
        let refusal: (@Sendable (String) async -> Void)?
        if conditionalRefusal { refusal = { await credentials.refuse($0) } } else { refusal = nil }
        let transport = try AlondraAPIClient.makeTransport(base: URL(string: "https://relay.fixture.invalid")!, deviceID: "phone-1",
            signer: { try signing.signature(for: $0).rawRepresentation }, bearer: { await credentials.value() },
            bearerRefused: { await credentials.refuse() },
            bearerRefusedForToken: refusal, transport: peer, now: { 1_000 })
        return (transport, peer, credentials)
    }
    private func heldStatus(_ transport: any Transport, peer: AuthorizationOrderingPeer) async -> Task<RelayJob, any Error> {
        let task = Task { try await transport.status(requestID: "held-A") }
        var arrival = peer.entered.makeAsyncIterator()
        _ = await arrival.next()
        return task
    }
    @Test func tokenRotationBeforeTheNextDispatchStillFencesAnOldRefusal() async throws {
        let (transport, peer, credentials) = try setup(conditionalRefusal: true)
        _ = try await transport.bootstrap()
        let old = await heldStatus(transport, peer: peer)
        await credentials.replace("fresh-token"); await peer.requireToken("fresh-token")
        await peer.release(status: 401)
        await #expect(throws: RelayWireError.unauthorized) { try await old.value }
        #expect(await credentials.refusals == 0)
        #expect(try await transport.status(requestID: "detail-B").requestID == "detail-B")
    }

    @Test func lateUnauthorizedCannotDiscardANewerProvenEpoch() async throws {
        let (transport, peer, credentials) = try setup()
        _ = try await transport.bootstrap()
        let old = await heldStatus(transport, peer: peer)
        await peer.changeEpoch()
        _ = try await transport.bootstrap()
        #expect(try await transport.status(requestID: "detail-B").requestID == "detail-B")
        await peer.prohibitBootstrap()
        await peer.release(status: 401)
        await #expect(throws: RelayWireError.unauthorized) { try await old.value }
        #expect(try await transport.status(requestID: "detail-B").requestID == "detail-B",
            "a late refusal of the old epoch must not require bootstrap again for the already verified new epoch")
        #expect(await credentials.refusals == 0)
    }
    @Test func lateSuccessCannotProveAnEpochItDidNotUse() async throws {
        let (transport, peer, credentials) = try setup()
        _ = try await transport.bootstrap()
        let old = await heldStatus(transport, peer: peer)
        await peer.changeEpoch()
        _ = try await transport.bootstrap()
        await peer.release(status: 200)
        #expect(try await old.value.requestID == "held-A")
        await peer.rejectNextStatus()
        await #expect(throws: RelayWireError.unauthorized) { try await transport.status(requestID: "detail-B") }
        #expect(await credentials.refusals == 1,
            "success under the old epoch cannot redirect a fresh-epoch bearer refusal into epoch recovery")
        await credentials.replace("fresh-token"); await peer.requireToken("fresh-token")
        await peer.prohibitBootstrap()
        #expect(try await transport.status(requestID: "detail-B").requestID == "detail-B")
    }
    @Test func lateUnauthorizedCannotRefuseANewerBearerOnTheSameEpoch() async throws {
        let (transport, peer, credentials) = try setup()
        _ = try await transport.bootstrap()
        let old = await heldStatus(transport, peer: peer)
        await credentials.replace("fresh-token"); await peer.requireToken("fresh-token")
        #expect(try await transport.status(requestID: "detail-B").requestID == "detail-B")
        _ = try await transport.bootstrap()
        await peer.release(status: 401)
        await #expect(throws: RelayWireError.unauthorized) { try await old.value }
        #expect(try await transport.status(requestID: "detail-B").requestID == "detail-B")
        #expect(await credentials.refusals == 0,
            "the refusal callback invalidates current credentials, so a response for an older bearer must not invoke it")
    }
}
