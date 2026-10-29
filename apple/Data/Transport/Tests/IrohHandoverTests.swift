@testable import AlondraTransport
import AlondraContract
import CryptoKit
import Foundation
import Synchronization
import Testing

private final class HandoverLink: IrohLink {
    private struct State {
        var helloSent = false
        var closed = false
        var losesResponses = false
        var holdNext = false
        var held: (CheckedContinuation<Data, any Error>, Data)?
        var paths: [AsyncStream<IrohPath>.Continuation] = []
    }
    private let state = Mutex(State())
    private let signing: P256.Signing.PrivateKey
    private let recipient: P256.KeyAgreement.PublicKey
    let exchangeHeld = AsyncStream<Void>.makeStream()

    init(signing: P256.Signing.PrivateKey, recipient: P256.KeyAgreement.PublicKey) {
        self.signing = signing; self.recipient = recipient
    }

    func holdNextExchange() { state.withLock { $0.holdNext = true } }
    func loseResponses() { state.withLock { $0.losesResponses = true } }

    func exchange(_ request: Data, answerLimit: Int) async throws -> Data {
        let hello = state.withLock { value in
            defer { value.helloSent = true }
            return !value.helloSent
        }
        if hello { return Data(#"{"ok":true}"#.utf8) }
        if state.withLock({ $0.losesResponses }) { throw IrohLinkError.noAnswer }
        let envelope = try JSONDecoder().decode(Envelope.self, from: request)
        let answer = try Envelope.seal(route: "mac", sender: "mac", recipient: "phone",
            requestID: envelope.requestID, kind: "response", expiresAtMilliseconds: 120_000,
            plaintext: Data(#"{"status":200,"body":{"ok":true}}"#.utf8), senderKey: signing,
            recipientKey: recipient)
        let reply = Data(#"{"response":"#.utf8) + (try JSONEncoder().encode(answer)) + Data("}".utf8)
        return try await withCheckedThrowingContinuation { continuation in
            let held = state.withLock { value -> Bool in
                guard !value.closed else {
                    continuation.resume(throwing: IrohLinkError.noAnswer)
                    return false
                }
                guard value.holdNext else {
                    continuation.resume(returning: reply)
                    return false
                }
                value.holdNext = false
                value.held = (continuation, reply)
                return true
            }
            if held { exchangeHeld.continuation.yield() }
        }
    }

    func releaseExchange() {
        let held = state.withLock { value in
            defer { value.held = nil }
            return value.held
        }
        if let (continuation, reply) = held { continuation.resume(returning: reply) }
    }

    func path() -> IrohPath { .relay }
    var isClosed: Bool { state.withLock { $0.closed } }

    func pathChanges() -> AsyncStream<IrohPath> {
        AsyncStream { continuation in
            let closed = state.withLock { value in
                if !value.closed { value.paths.append(continuation) }
                return value.closed
            }
            if closed { continuation.finish() }
        }
    }

    func close() async {
        let pending = state.withLock { value in
            value.closed = true
            defer { value.held = nil; value.paths.removeAll() }
            return (value.held, value.paths)
        }
        pending.0?.0.resume(throwing: IrohLinkError.noAnswer)
        for path in pending.1 { path.finish() }
    }
}

private actor HandoverDialer: IrohDialer {
    nonisolated let endpointID = IrohIdentity(seed: Data(repeating: 4, count: 32)).endpointID
    nonisolated let dialHeld = AsyncStream<Void>.makeStream()
    private let signing: P256.Signing.PrivateKey
    private let recipient: P256.KeyAgreement.PublicKey
    private let holdFirst: Bool
    private var held: CheckedContinuation<Void, Never>?
    private var links: [HandoverLink] = []

    init(signing: P256.Signing.PrivateKey, recipient: P256.KeyAgreement.PublicKey, holdFirst: Bool) {
        self.signing = signing; self.recipient = recipient; self.holdFirst = holdFirst
    }

    var first: HandoverLink? { links.first }
    var dials: Int { links.count }

    func dial(_ mac: IrohMacAddress) async throws -> any IrohLink {
        let link = HandoverLink(signing: signing, recipient: recipient)
        links.append(link)
        if holdFirst, links.count == 1 {
            await withCheckedContinuation { continuation in
                held = continuation
                dialHeld.continuation.yield()
            }
        }
        return link
    }

    func releaseDial() {
        let waiting = held; held = nil
        waiting?.resume()
    }

    func networkMoved() async {}

    func shutdown() async {
        releaseDial()
        for link in links { await link.close() }
    }
}

private struct HandoverFixture {
    let carrier: IrohCarrier
    let session: DirectSession
    let dialer: HandoverDialer

    init(holdFirstDial: Bool = false) throws {
        let deviceSigning = try P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 1, count: 32))
        let deviceAgreement = try P256.KeyAgreement.PrivateKey(rawRepresentation: Data(repeating: 2, count: 32))
        let macSigning = try P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 3, count: 32))
        let macAgreement = try P256.KeyAgreement.PrivateKey(rawRepresentation: Data(repeating: 4, count: 32))
        let macEndpoint = IrohIdentity(seed: Data(repeating: 5, count: 32)).endpointIDHex
        dialer = HandoverDialer(signing: macSigning, recipient: deviceAgreement.publicKey, holdFirst: holdFirstDial)
        carrier = IrohCarrier(dialer: dialer, macID: "mac", deviceID: "phone", signing: deviceSigning,
            address: { IrohMacAddress(endpointID: macEndpoint, relay: URL(string: "https://relay.example.com")) },
            lingerAfterUse: .seconds(3_600))
        session = DirectSession(macID: "mac", deviceID: "phone", signing: deviceSigning, agreement: deviceAgreement,
            macSigning: macSigning.publicKey, macAgreement: macAgreement.publicKey, iroh: carrier, now: { 1_000 })
    }

    func request(method: String = "GET") throws -> APIRequest {
        try APIRequest(endpoint: Endpoint<Data>.data(method == "GET" ? .get : .post,
            path: method == "GET" ? ["capabilities"] : ["actions"],
            body: method == "GET" ? nil : Data(#"{"command":"answer"}"#.utf8)), operationID: "answer")
    }
}

struct IrohHandoverTests {
    @Test(arguments: ["GET", "POST"])
    func goingToTheBackgroundKeepsAnExchangeInFlight(method: String) async throws {
        let fixture = try HandoverFixture()
        await fixture.carrier.start()
        do {
            _ = try await fixture.session.perform(fixture.request())
            let link = try #require(await fixture.dialer.first)
            link.holdNextExchange()
            var exchanges = link.exchangeHeld.stream.makeAsyncIterator()
            let request = try fixture.request(method: method)
            let delivery = Task { try await fixture.session.perform(request) }
            await exchanges.next()

            await fixture.carrier.stop()
            #expect(!link.isClosed, "backgrounding must let the answer already on the relay finish")
            link.releaseExchange()

            #expect(try await delivery.value == .completed(status: 200, body: Data(#"{"ok":true}"#.utf8)))
            #expect(await fixture.dialer.dials == 1, "the in-flight response must use its original connection")
            await fixture.carrier.shutdown()
        } catch {
            await fixture.carrier.shutdown()
            throw error
        }
    }

    @Test func theForegroundKeepsAConnectionStillBeingOpenedOnDemand() async throws {
        let fixture = try HandoverFixture(holdFirstDial: true)
        var dials = fixture.dialer.dialHeld.stream.makeAsyncIterator()
        let request = try fixture.request()
        let delivery = Task { try await fixture.session.perform(request) }
        await dials.next()

        await fixture.carrier.start()
        await fixture.dialer.releaseDial()

        do {
            #expect(try await delivery.value == .completed(status: 200, body: Data(#"{"ok":true}"#.utf8)))
            let original = try #require(await fixture.dialer.first)
            #expect(!original.isClosed, "a healthy connection being opened must survive foregrounding")
            #expect(await fixture.dialer.dials == 1, "foregrounding must share the dial already in progress")
            await fixture.carrier.shutdown()
        } catch {
            await fixture.carrier.shutdown()
            throw error
        }
    }

    @Test func goingToTheBackgroundKeepsTheConnectionAnAnswerIsStillOpening() async throws {
        let fixture = try HandoverFixture(holdFirstDial: true)
        var dials = fixture.dialer.dialHeld.stream.makeAsyncIterator()
        let request = try fixture.request(method: "POST")
        let delivery = Task { try await fixture.session.perform(request) }
        await dials.next()

        await fixture.carrier.start()
        await fixture.carrier.stop()
        await fixture.dialer.releaseDial()

        do {
            #expect(try await delivery.value == .completed(status: 200, body: Data(#"{"ok":true}"#.utf8)))
            #expect(await fixture.dialer.dials == 1, "backgrounding must preserve the dial the answer is awaiting")
            await fixture.carrier.shutdown()
        } catch {
            await fixture.carrier.shutdown()
            throw error
        }
    }

    @Test func shuttingDownClosesAnExchangeInFlight() async throws {
        let fixture = try HandoverFixture()
        await fixture.carrier.start()
        do {
            _ = try await fixture.session.perform(fixture.request())
            let link = try #require(await fixture.dialer.first)
            link.holdNextExchange()
            var exchanges = link.exchangeHeld.stream.makeAsyncIterator()
            let request = try fixture.request(method: "POST")
            let delivery = Task { try await fixture.session.perform(request) }
            await exchanges.next()

            await fixture.carrier.shutdown()

            #expect(link.isClosed, "shutdown must close a connection even when an answer is pending")
            #expect(try await delivery.value == .unknown, "a closed stream cannot confirm the pending answer")
            #expect(await fixture.dialer.dials == 1, "shutdown must not open a replacement connection")
        } catch {
            await fixture.carrier.shutdown()
            throw error
        }
    }

    @Test func anUnansweredStreamDoesNotKeepLaterRequestsOnAnUnresponsiveConnection() async throws {
        let fixture = try HandoverFixture()
        await fixture.carrier.start()
        do {
            _ = try await fixture.session.perform(fixture.request())
            let stale = try #require(await fixture.dialer.first)
            stale.loseResponses()
            #expect(!stale.isClosed, "a failed stream need not report that the QUIC connection closed")

            #expect(try await fixture.session.perform(fixture.request(method: "POST")) == .unknown)

            #expect(try await fixture.session.perform(fixture.request()) ==
                .completed(status: 200, body: Data(#"{"ok":true}"#.utf8)))
            #expect(await fixture.dialer.dials == 2, "the next request must leave on a fresh connection")
            await fixture.carrier.shutdown()
        } catch {
            await fixture.carrier.shutdown()
            throw error
        }
    }
}
