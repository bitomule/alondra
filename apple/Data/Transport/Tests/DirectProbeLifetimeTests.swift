@testable import AlondraTransport
import CryptoKit
import Foundation
import Synchronization
import Testing

@Suite struct DirectProbeLifetimeTests {
    @Test(arguments: ProbeInvalidation.allCases) func anInvalidatedAnswerCannotValidateTheCurrentNetwork(invalidation: ProbeInvalidation) async throws {
        let identity = DirectMacIdentity(macID: "mac-fixture", name: "Fixture",
            signingKey: Base64URL.encode(P256.Signing.PrivateKey().publicKey.x963Representation),
            agreementKey: Base64URL.encode(P256.KeyAgreement.PrivateKey().publicKey.x963Representation))
        try #require(identity.isValid, "the fixture must reach route selection")
        let peer = HeldProbePeer(identity: identity)
        let network = ProbeNetwork()
        let endpoint = DirectEndpoint(host: "192.0.2.1", port: 47810, kind: .localNetwork)
        let router = DirectRouter(pinned: identity, saved: [endpoint], http: DirectHTTP(transport: peer), network: { network.value })
        var starts = await peer.starts().makeAsyncIterator()
        let pending = Task { try await router.endpoint() }
        #expect(await starts.next() == endpoint.host)
        if invalidation != .sameNetwork { network.change(to: "new-network") }
        if invalidation != .networkOnly { await router.invalidate() }
        try await peer.release()
        let discarded: Bool
        switch await pending.result {
        case .success:
            Issue.record("an invalidated hello must not validate this route")
            discarded = false
        case let .failure(error):
            #expect(error is CancellationError || (error as? DirectRouteError) == .unreachable)
            discarded = true
        }
        #expect(await router.route == nil)
        #expect(await !router.isProven(endpoint))
        guard discarded else { return }
        let fresh = Task { try await router.endpoint() }
        #expect(await starts.next() == endpoint.host)
        try await peer.release()
        #expect(try await fresh.value == endpoint)
        #expect(await router.route == endpoint.route)
        #expect(await !router.isProven(endpoint), "hello alone cannot confer cryptographic trust")
    }

    @Test func anAnswerIsStampedWithTheNetworkThatWasProbed() async throws {
        let identity = DirectMacIdentity(macID: "mac-fixture", name: "Fixture",
            signingKey: Base64URL.encode(P256.Signing.PrivateKey().publicKey.x963Representation),
            agreementKey: Base64URL.encode(P256.KeyAgreement.PrivateKey().publicKey.x963Representation))
        try #require(identity.isValid)
        let peer = AnsweringProbePeer(identity: identity)
        let reads = Mutex(0)
        let endpoint = DirectEndpoint(host: "192.0.2.1", port: 47810, kind: .localNetwork)
        let router = DirectRouter(pinned: identity, saved: [endpoint], http: DirectHTTP(transport: peer), network: {
            reads.withLock { count in
                count += 1
                return count <= 2 ? "old-network" : "new-network"
            }
        })
        #expect(try await router.endpoint() == endpoint)
        #expect(try await router.endpoint() == endpoint)
        #expect(await peer.calls == 2, "the old hello cannot be cached as validated on the new network")
        #expect(await !router.isProven(endpoint))
    }

}

enum ProbeInvalidation: CaseIterable, Sendable {
    case sameNetwork, networkOnly, changedNetwork
}

private final class ProbeNetwork: Sendable {
    private let name = Mutex("old-network")
    var value: String { name.withLock { $0 } }
    func change(to value: String) { name.withLock { $0 = value } }
}

private actor HeldProbePeer: HTTPTransport {
    private let identity: DirectMacIdentity
    private let stream: AsyncStream<String>
    private let continuation: AsyncStream<String>.Continuation
    private var pending: CheckedContinuation<HTTPReply, any Error>?

    init(identity: DirectMacIdentity) {
        self.identity = identity
        let pair = AsyncStream<String>.makeStream()
        stream = pair.stream; continuation = pair.continuation
    }

    func starts() -> AsyncStream<String> { stream }

    func send(_ request: URLRequest) async throws -> HTTPReply {
        try await withCheckedThrowingContinuation { completion in
            pending = completion
            continuation.yield(request.url!.host()!)
        }
    }

    func release() throws {
        let completion = pending
        pending = nil
        completion?.resume(returning: HTTPReply(status: 200, body: try JSONEncoder().encode(identity)))
    }
}

private actor AnsweringProbePeer: HTTPTransport {
    private let identity: DirectMacIdentity
    private(set) var calls = 0

    init(identity: DirectMacIdentity) { self.identity = identity }

    func send(_ request: URLRequest) async throws -> HTTPReply {
        calls += 1
        return HTTPReply(status: 200, body: try JSONEncoder().encode(identity))
    }
}
