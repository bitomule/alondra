@testable import AlondraTransport
import AlondraContract
import CryptoKit
import Darwin
import Foundation
import Network
import Synchronization
import Testing

private let freshnessLAN = DirectEndpoint(host: "192.0.2.10", port: 47810, kind: .localNetwork)
private let freshnessOtherLAN = DirectEndpoint(host: "192.0.2.11", port: 47810, kind: .localNetwork)
private let freshnessTailnet = DirectEndpoint(host: "100.64.0.10", port: 47810, kind: .tailscale)
private let freshnessBody = Data(#"{"key":"card:controlled","protocol":1,"text":"Controlled card"}"#.utf8)
private let freshnessAnswer = APIResponse.completed(status: 200, body: freshnessBody)

@Suite(.serialized) struct DirectRouteFreshnessTests {
    @Test func theFirstSealedReadFinishesBeforeAnUnusedHelloIsReleased() async throws {
        let observed = AsyncStream<DirectProbeObservation>.makeStream()
        let f = try FreshnessFixture(probeObserved: { observed.continuation.yield($0) })
        await f.peer.holdHello(on: freshnessLAN.host)
        await f.peer.holdHello(on: freshnessTailnet.host)
        var entered = await f.peer.observe().makeAsyncIterator()
        let read = Task { try await f.read() }
        var hosts = Set<String>()
        for _ in 0..<2 {
            if case let .hello(host) = await entered.next() { hosts.insert(host) }
        }
        #expect(hosts == [freshnessLAN.host, freshnessTailnet.host])
        await f.peer.releaseHello(on: freshnessLAN.host)
        var processed = observed.stream.makeAsyncIterator()
        while await processed.next() != .answered(freshnessLAN) {}
        let selected = await f.router.route == .localNetwork
        if selected {
            await expectFreshnessAnswer(read.result)
            #expect(await f.router.isProven(freshnessLAN), "the first answer must open with the pinned keys before the unused hello ends")
            #expect(await f.peer.stats().pending == 1)
        }
        await f.peer.releaseAll()
        await f.router.drainProbes()
        if !selected { await expectFreshnessAnswer(read.result) }
        #expect(selected, "a processed preferred hello must permit the sealed read while the unused lower preference hello is still held")
        #expect(await f.peer.stats().reads == 1)
        #expect(await f.peer.stats().effects == 0)
        #expect(await f.peer.stats().pending == 0)
        await f.session.deactivate()
    }

    @Test func anInvalidatedWinnerWaitsForPhysicalDrainBeforeProbingAgain() async throws {
        let observed = AsyncStream<DirectProbeObservation>.makeStream()
        let f = try FreshnessFixture(probeObserved: { observed.continuation.yield($0) })
        await f.peer.holdHello(on: freshnessLAN.host)
        await f.peer.holdHello(on: freshnessTailnet.host)
        var entered = await f.peer.observe().makeAsyncIterator()
        let first = Task { try await f.read() }
        for _ in 0..<2 { _ = await entered.next() }
        await f.peer.releaseHello(on: freshnessLAN.host)
        await expectFreshnessAnswer(first.result)
        await f.router.invalidate()
        var events = observed.stream.makeAsyncIterator()
        let replacement = Task { try await f.read() }
        while await events.next() != .waitingForDrain {}
        #expect(await f.peer.stats().hellos.values.reduce(0, +) == 2)
        #expect(await f.peer.stats().pending == 1)
        await f.peer.releaseAll()
        await expectFreshnessAnswer(replacement.result)
        await f.router.drainProbes()
        #expect(await f.peer.stats().reads == 2)
        #expect(await f.peer.stats().hellos[freshnessLAN.host] == 2)
        #expect(await f.peer.stats().hellos.values.reduce(0, +) <= 4)
        #expect(await f.router.isProven(freshnessLAN))
        #expect(await f.peer.stats().pending == 0)
        await f.session.deactivate()
    }

    @Test func aNewReadWaitsForPhysicalDrainAfterTheOldSnapshotFailed() async throws {
        let name = DirectEndpoint(host: "fixture.tail.ts.net", port: 47810, kind: .tailscale)
        let observed = AsyncStream<DirectProbeObservation>.makeStream()
        let f = try FreshnessFixture(saved: [freshnessTailnet, name], probeObserved: { observed.continuation.yield($0) })
        await f.peer.holdHello(on: freshnessTailnet.host)
        await f.peer.holdHello(on: name.host)
        var entered = await f.peer.observe().makeAsyncIterator()
        var progress = observed.stream.makeAsyncIterator()
        let old = Task { try await f.read() }
        for _ in 0..<2 { _ = await entered.next() }
        await f.router.offer([freshnessLAN], from: .mac)
        await f.peer.releaseHello(on: freshnessTailnet.host)
        while await progress.next() != .answered(freshnessTailnet) {}
        switch await old.result {
        case let .failure(error): #expect((error as? DirectRouteError) == .unreachable)
        case .success: Issue.record("the invalidated old snapshot must not dispatch")
        }
        let replacement = Task { try await f.read() }
        var next = await progress.next()
        while next != .waitingForDrain && next != .joiningProbe { next = await progress.next() }
        let reusedFailure: Result<APIResponse, any Error>?
        if next == .joiningProbe { reusedFailure = await replacement.result }
        else { reusedFailure = nil }
        #expect(await f.peer.stats().hellos.values.reduce(0, +) == 2)
        #expect(await f.peer.stats().pending == 1)
        await f.peer.releaseAll()
        if let reusedFailure { await expectFreshnessAnswer(reusedFailure) }
        else { await expectFreshnessAnswer(replacement.result) }
        await f.router.drainProbes()
        #expect(await f.router.route == .localNetwork)
        #expect(await f.router.isProven(freshnessLAN))
        #expect(await f.peer.stats().reads == 1)
        #expect(await f.peer.stats().pending == 0)
        await f.session.deactivate()
    }

    @Test func cancellingOneProbeWaiterDoesNotCancelTheSharedDiscovery() async throws {
        let observed = AsyncStream<DirectProbeObservation>.makeStream()
        let f = try FreshnessFixture(saved: [freshnessLAN], probeObserved: { observed.continuation.yield($0) })
        await f.peer.holdHello(on: freshnessLAN.host)
        var entered = await f.peer.observe().makeAsyncIterator()
        var events = observed.stream.makeAsyncIterator()
        let first = Task { try await f.read() }
        #expect(await entered.next() == .hello(freshnessLAN.host))
        while await events.next() != .waitingForProbe {}
        let cancelled = Task { try await f.router.endpoint() }
        while await events.next() != .waitingForProbe {}
        #expect(await f.router.probeWaiterCount == 2)
        cancelled.cancel()
        #expect(await f.router.probeWaiterCount == 1, "cancellation must remove the waiter before the physical hello exits")
        await f.peer.releaseAll()
        switch await cancelled.result {
        case let .failure(error): #expect(error is CancellationError)
        case .success: Issue.record("the cancelled waiter cannot dispatch using the shared answer")
        }
        await expectFreshnessAnswer(first.result)
        await f.router.drainProbes()
        #expect(await f.router.probeWaiterCount == 0)
        #expect(await f.peer.stats().reads == 1)
        #expect(await f.peer.stats().hellos.values.reduce(0, +) == 1)
        #expect(await f.peer.stats().pending == 0)
        await f.session.deactivate()
    }

    @Test func aHigherPreferenceOfferInvalidatesTheFirstCandidateSnapshot() async throws {
        let f = try FreshnessFixture(saved: [freshnessTailnet])
        await f.peer.holdHello(on: freshnessTailnet.host)
        var entered = await f.peer.observe().makeAsyncIterator()
        let old = Task { try await f.read() }
        #expect(await entered.next() == .hello(freshnessTailnet.host))
        await f.router.offer([freshnessLAN], from: .mac)
        await f.peer.releaseAll()
        switch await old.result {
        case let .failure(error): #expect((error as? DirectRouteError) == .unreachable)
        case .success: Issue.record("an initial probe must not promote the old lower preference snapshot")
        }
        await f.router.drainProbes()
        #expect(await f.peer.stats().reads == 0)
        #expect(try await f.read() == freshnessAnswer)
        await f.router.drainProbes()
        #expect(await f.router.route == .localNetwork)
        #expect(await f.router.isProven(freshnessLAN))
        #expect(await f.peer.stats().reads == 1)
        #expect(await f.peer.stats().pending == 0)
        await f.session.deactivate()
    }

    @Test func aFastTailnetHelloStillWaitsForThePreferredLAN() async throws {
        let observed = AsyncStream<DirectProbeObservation>.makeStream()
        let f = try FreshnessFixture(probeObserved: { observed.continuation.yield($0) })
        await f.peer.holdHello(on: freshnessLAN.host)
        var consumed = observed.stream.makeAsyncIterator()
        let read = Task { try await f.read() }
        while await consumed.next() != .answered(freshnessTailnet) {}
        #expect(await f.router.route == nil)
        #expect(await f.peer.stats().reads == 0)
        await f.peer.releaseHello(on: freshnessLAN.host)
        await expectFreshnessAnswer(read.result)
        await f.router.drainProbes()
        #expect(await f.router.route == .localNetwork)
        #expect(await f.router.isProven(freshnessLAN))
        #expect(await f.peer.stats().pending == 0)
        await f.session.deactivate()
    }

    @Test func anActiveReadDoesNotWaitForAnUnusedAddress() async throws {
        let f = try FreshnessFixture()
        #expect(try await f.read() == freshnessAnswer)
        f.time.set(seconds: 59)
        #expect(try await f.read() == freshnessAnswer)
        f.time.set(seconds: 61)
        await f.peer.holdHello(on: freshnessTailnet.host)
        var events = await f.peer.observe().makeAsyncIterator()
        let pending = Task { try await f.read() }
        let first = await events.next()
        #expect(first == .request(freshnessLAN.host), "an active read must dispatch without waiting for an unused address")
        await f.peer.releaseAll()
        await expectFreshnessAnswer(pending.result)
        #expect(await f.router.route == .localNetwork)
        let stats = await f.peer.stats()
        #expect(stats.reads == 3 && stats.effects == 0 && stats.pending == 0)
        #expect((1...2).contains(stats.hellos.values.reduce(0, +)))
    }

    @Test(arguments: FreshnessReply.failures) fileprivate func unsuccessfulRepliesDoNotRenewTheRoute(reply: FreshnessReply) async throws {
        let f = try FreshnessFixture(saved: [freshnessLAN])
        #expect(try await f.read() == freshnessAnswer)
        f.time.set(seconds: 59)
        await f.peer.respond(with: reply)
        let result = await Task { try await f.read() }.result
        switch reply {
        case .unauthorized, .upgradeRequired:
            if case let .success(response) = result { #expect(response == .completed(status: reply.status, body: freshnessBody)) }
            else { Issue.record("a verified API refusal must retain its existing result") }
        case .revoked:
            if case let .failure(error) = result { #expect((error as? DirectSessionError) == .deviceRevoked) }
            else { Issue.record("a verified revocation must remain an error") }
        case .serverFailure, .uncertain:
            if case let .success(response) = result { #expect(response == .unknown) }
            else { Issue.record("an uncertain response must retain its existing result") }
        case .unsupportedEnvelope, .wrongKeys, .unsigned:
            if case let .failure(error) = result { #expect((error as? DirectRouteError) == .unreachable) }
            else { Issue.record("an unverified response cannot complete a request") }
            #expect(await f.router.route == nil)
            #expect(await !f.router.isProven(freshnessLAN))
        case .success: Issue.record("the failure table must exclude success")
        }
        await f.peer.respond(with: .success)
        f.time.set(seconds: 61)
        if reply.unverified { await f.router.offer([freshnessOtherLAN], from: .mac) }
        var events = await f.peer.observe().makeAsyncIterator()
        let pending = Task { try await f.read() }
        let first = await events.next()
        if case .some(.hello(_)) = first {} else { Issue.record("an unsuccessful reply cannot extend validation") }
        await expectFreshnessAnswer(pending.result)
        #expect(await f.peer.stats().pending == 0)
    }

    @Test(arguments: FreshnessTransition.allCases) fileprivate func aLateReplyCannotReviveAnEndedOrChangedRoute(transition: FreshnessTransition) async throws {
        let endpoint = transition == .higherOffer ? freshnessTailnet : freshnessLAN
        let f = try FreshnessFixture(saved: [endpoint])
        #expect(try await f.read() == freshnessAnswer)
        f.time.set(seconds: 59)
        await f.peer.holdResponses()
        var events = await f.peer.observe().makeAsyncIterator()
        let pending = Task { try await f.read() }
        #expect(await events.next() == .request(endpoint.host))
        switch transition {
        case .cancel: pending.cancel()
        case .deactivate: await f.session.deactivate()
        case .networkOnly: f.time.join("new-network")
        case .invalidate: await f.router.invalidate()
        case .networkAndInvalidate: f.time.join("new-network"); await f.router.invalidate()
        case .higherOffer: await f.router.offer([freshnessLAN], from: .mac)
        }
        await f.peer.releaseAll()
        let result = await pending.result
        if transition == .deactivate {
            if case let .failure(error) = result { #expect(error is CancellationError) }
            else { Issue.record("a deactivated session cannot complete its pending read") }
        } else {
            await expectFreshnessAnswer(result)
        }
        f.time.set(seconds: 61)
        var nextEvents = await f.peer.observe().makeAsyncIterator()
        let replacement = transition == .deactivate ? f.makeSession() : f.session
        let next = Task { try await replacement.perform(f.request()) }
        if case .some(.hello(_)) = await nextEvents.next() {} else { Issue.record("the late reply cannot renew an ended validation") }
        await expectFreshnessAnswer(next.result)
        if transition == .higherOffer { #expect(await f.router.route == .localNetwork) }
        #expect(await f.peer.stats().pending == 0)
    }

    @Test func anOldReplyCannotRenewAReplacementValidationOfTheSameEndpoint() async throws {
        let f = try FreshnessFixture(saved: [freshnessTailnet])
        #expect(try await f.read() == freshnessAnswer)
        f.time.set(seconds: 59)
        await f.peer.holdResponses()
        var events = await f.peer.observe().makeAsyncIterator()
        let old = Task { try await f.read() }
        #expect(await events.next() == .request(freshnessTailnet.host))
        await f.peer.impostorHello(on: freshnessLAN.host)
        await f.router.offer([freshnessLAN], from: .mac)
        #expect(try await f.router.endpoint() == freshnessTailnet)
        f.time.set(seconds: 119)
        await f.peer.releaseAll()
        await expectFreshnessAnswer(old.result)
        var nextEvents = await f.peer.observe().makeAsyncIterator()
        let next = Task { try await f.read() }
        if case .some(.hello(_)) = await nextEvents.next() {} else { Issue.record("a response from the old validation cannot extend the replacement") }
        await expectFreshnessAnswer(next.result)
        #expect(await f.peer.stats().pending == 0)
    }

    @Test func anOfferDuringAProbeDiscardsTheOldCandidateSnapshot() async throws {
        let f = try FreshnessFixture(saved: [freshnessTailnet])
        #expect(try await f.read() == freshnessAnswer)
        f.time.set(seconds: 60)
        await f.peer.holdHello(on: freshnessTailnet.host)
        var events = await f.peer.observe().makeAsyncIterator()
        let old = Task { try await f.read() }
        #expect(await events.next() == .hello(freshnessTailnet.host))
        await f.router.offer([freshnessLAN], from: .mac)
        await f.peer.releaseAll()
        if case let .failure(error) = await old.result { #expect((error as? DirectRouteError) == .unreachable) }
        else { Issue.record("an offer-invalidated hello batch must not dispatch its old selection") }
        #expect(await f.peer.stats().reads == 1)
        #expect(try await f.read() == freshnessAnswer)
        #expect(await f.router.route == .localNetwork)
        #expect(try await f.read() == freshnessAnswer)
        #expect(await f.peer.stats().pending == 0)
    }

    @Test func aReplacementProbeKeepsItsSelectionAfterInvalidation() async throws {
        let f = try FreshnessFixture(saved: [freshnessLAN])
        await f.peer.holdHello(on: freshnessLAN.host)
        var events = await f.peer.observe().makeAsyncIterator()
        let old = Task { try await f.router.endpoint() }
        #expect(await events.next() == .hello(freshnessLAN.host))
        await f.router.invalidate()
        await f.peer.releaseAll()
        if case let .failure(error) = await old.result { #expect((error as? DirectRouteError) == .unreachable) }
        else { Issue.record("the invalidated probe must be discarded before replacing it") }
        #expect(try await f.read() == freshnessAnswer)
        let before = await f.peer.stats().hellos
        #expect(try await f.read() == freshnessAnswer)
        #expect(await f.router.route == .localNetwork)
        #expect(await f.peer.stats().hellos == before)
    }

    @Test func anIdleDirectRouteStillExpires() async throws {
        let f = try FreshnessFixture(saved: [freshnessLAN])
        #expect(try await f.read() == freshnessAnswer)
        f.time.set(seconds: 60)
        var events = await f.peer.observe().makeAsyncIterator()
        let pending = Task { try await f.read() }
        #expect(await events.next() == .hello(freshnessLAN.host))
        await expectFreshnessAnswer(pending.result)
    }

    @Test(.enabled(if: ProcessInfo.processInfo.environment["ALONDRA_DIRECT_ROUTE_PROFILE"] == "1"))
    func repeatedAuthenticatedReads() async throws {
        let f = try FreshnessFixture()
        let before = freshnessResources()
        var completed = 0
        var bodies = Set<String>()
        for second in 0...60 {
            f.time.set(seconds: second)
            let response = try await f.read()
            try #require(response == freshnessAnswer)
            if case let .completed(_, body) = response {
                bodies.insert(SHA256.hash(data: body).map { String(format: "%02x", $0) }.joined())
            }
            completed += 1
        }
        let after = freshnessResources()
        let stats = await f.peer.stats()
        #expect(completed == 61 && stats.reads == 61 && stats.effects == 0 && stats.pending == 0)
        #expect(bodies.count == 1)
        #expect(await f.router.route == .localNetwork)
        let result: [String: Any] = [
            "profile": "controlled-warm-direct", "offered": 61, "completed": completed, "errors": 0,
            "hello_by_host": stats.hellos, "direct_requests": stats.reads, "mutation_effects": stats.effects,
            "pending_gates": stats.pending, "request_body_bytes": stats.requestBytes,
            "response_body_bytes": stats.responseBytes, "decoded_body_sha256": bodies.sorted(),
            "selected_route": "localNetwork", "process_cpu_ms": after.cpu - before.cpu,
            "process_lifetime_maxrss_bytes_before": before.rss, "process_lifetime_maxrss_bytes_after": after.rss,
            "limits": "Controlled cache age t0...t60 and real CryptoKit envelopes against owned in-memory HTTP peers. No actual network, daemon, relay, Keychain, installed app or hosted server. CPU covers only 61 requests; lifetime RSS includes test runtime/fixtures. Held-gate tests prove dispatch ordering; artificial time is not RTT or end-user latency. Ciphertext/read IDs vary; plaintext body/effects are the oracle.",
        ]
        let data = try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys])
        print("DIRECT_ROUTE_MEASUREMENT " + String(decoding: data, as: UTF8.self))
    }
}

private func expectFreshnessAnswer(_ result: Result<APIResponse, any Error>) async {
    switch result {
    case let .success(response): #expect(response == freshnessAnswer)
    case let .failure(error): Issue.record("expected the original card bytes, got \(error)")
    }
}

private final class FreshnessTime: Sendable {
    private struct State {
        let origin = ContinuousClock().now
        var second = 0
        var network = "old-network"
    }
    private let state = Mutex(State())
    var instant: ContinuousClock.Instant { state.withLock { $0.origin.advanced(by: .seconds($0.second)) } }
    var milliseconds: UInt64 { state.withLock { 2_000_000_000_000 + UInt64($0.second) * 1_000 } }
    var network: String { state.withLock { $0.network } }
    func set(seconds: Int) { state.withLock { $0.second = seconds } }
    func join(_ network: String) { state.withLock { $0.network = network } }
}

private struct FreshnessKeys: Sendable {
    let mac: P256.Signing.PrivateKey
    let macAgreement: P256.KeyAgreement.PrivateKey
    let device: P256.Signing.PrivateKey
    let deviceAgreement: P256.KeyAgreement.PrivateKey
    let impostor: P256.Signing.PrivateKey
    init() throws {
        mac = try .init(rawRepresentation: Data(repeating: 1, count: 32))
        macAgreement = try .init(rawRepresentation: Data(repeating: 2, count: 32))
        device = try .init(rawRepresentation: Data(repeating: 3, count: 32))
        deviceAgreement = try .init(rawRepresentation: Data(repeating: 4, count: 32))
        impostor = try .init(rawRepresentation: Data(repeating: 5, count: 32))
    }
    var identity: DirectMacIdentity {
        .init(macID: "mac-fixture", name: "Fixture", signingKey: Base64URL.encode(mac.publicKey.x963Representation),
              agreementKey: Base64URL.encode(macAgreement.publicKey.x963Representation))
    }
}

private struct FreshnessFixture {
    let time = FreshnessTime()
    let keys: FreshnessKeys
    let peer: FreshnessPeer
    let router: DirectRouter
    let session: DirectSession
    init(saved: [DirectEndpoint] = [freshnessLAN, freshnessTailnet], probeObserved: (@Sendable (DirectProbeObservation) -> Void)? = nil) throws {
        let time = time
        let keys = try FreshnessKeys()
        self.keys = keys
        peer = FreshnessPeer(keys: keys, time: time)
        let http = DirectHTTP(transport: peer)
        router = DirectRouter(pinned: keys.identity, saved: saved, http: http,
                              network: { time.network }, now: { time.instant }, probeObserved: probeObserved)
        session = Self.session(keys: keys, time: time, router: router, peer: peer)
    }
    func makeSession() -> DirectSession { Self.session(keys: keys, time: time, router: router, peer: peer) }
    private static func session(keys: FreshnessKeys, time: FreshnessTime, router: DirectRouter, peer: FreshnessPeer) -> DirectSession {
        .init(macID: "mac-fixture", deviceID: "phone-fixture", signing: keys.device, agreement: keys.deviceAgreement,
              macSigning: keys.mac.publicKey, macAgreement: keys.macAgreement.publicKey, router: router,
              http: DirectHTTP(transport: peer), now: { time.milliseconds }, pause: { _ in })
    }
    func request() throws -> APIRequest {
        try APIRequest(endpoint: Endpoint<Data>.data(.get, path: ["views", "card:controlled"],
            query: [("protocol", "1"), ("client", "mac"), ("lang", "en")]), operationID: "controlled-read")
    }
    func read() async throws -> APIResponse { try await session.perform(request()) }
}

private enum FreshnessReply: Sendable, CaseIterable {
    case success, unauthorized, upgradeRequired, revoked, serverFailure, uncertain, unsupportedEnvelope, wrongKeys, unsigned
    static var failures: [Self] { allCases.filter { $0 != .success } }
    var status: Int {
        switch self {
        case .unauthorized: 401
        case .upgradeRequired: 426
        case .revoked: 403
        case .serverFailure: 503
        default: 200
        }
    }
    var unverified: Bool { self == .unsupportedEnvelope || self == .wrongKeys || self == .unsigned }
}

private enum FreshnessTransition: CaseIterable, Sendable {
    case cancel, deactivate, networkOnly, invalidate, networkAndInvalidate, higherOffer
}

private actor FreshnessPeer: HTTPTransport {
    enum Event: Sendable, Equatable { case hello(String), request(String) }
    struct Stats: Sendable {
        let hellos: [String: Int]
        let reads: Int
        let effects: Int
        let requestBytes: Int
        let responseBytes: Int
        let pending: Int
    }
    private let keys: FreshnessKeys
    private let time: FreshnessTime
    private var reply: FreshnessReply = .success
    private var impostorHosts: Set<String> = []
    private var heldHello: Set<String> = []
    private var heldResponses = false
    private var pending: [(CheckedContinuation<HTTPReply, Never>, HTTPReply, Event)] = []
    private var observer: AsyncStream<Event>.Continuation?
    private var hellos: [String: Int] = [:]
    private var reads = 0
    private var effects = 0
    private var requestBytes = 0
    private var responseBytes = 0
    init(keys: FreshnessKeys, time: FreshnessTime) { self.keys = keys; self.time = time }
    func respond(with reply: FreshnessReply) { self.reply = reply }
    func holdHello(on host: String) { heldHello.insert(host) }
    func holdResponses() { heldResponses = true }
    func impostorHello(on host: String) { impostorHosts.insert(host) }
    func observe() -> AsyncStream<Event> {
        observer?.finish()
        let pair = AsyncStream<Event>.makeStream()
        observer = pair.continuation
        return pair.stream
    }
    func stats() -> Stats {
        .init(hellos: hellos, reads: reads, effects: effects, requestBytes: requestBytes,
              responseBytes: responseBytes, pending: pending.count)
    }
    func releaseHello(on host: String) {
        heldHello.remove(host)
        let released = pending.filter { $0.2 == .hello(host) }
        pending.removeAll { $0.2 == .hello(host) }
        for (continuation, response, _) in released { continuation.resume(returning: response) }
    }
    func releaseAll() {
        heldHello.removeAll(); heldResponses = false
        let waiting = pending
        pending.removeAll()
        for (continuation, response, _) in waiting { continuation.resume(returning: response) }
    }
    func send(_ request: URLRequest) async throws -> HTTPReply {
        let url = try #require(request.url)
        let host = try #require(url.host())
        if url.path == "/direct/hello" {
            hellos[host, default: 0] += 1
            let identity = impostorHosts.contains(host)
                ? DirectMacIdentity(macID: "mac-fixture", name: "Fixture", signingKey: Base64URL.encode(keys.impostor.publicKey.x963Representation),
                                    agreementKey: Base64URL.encode(keys.macAgreement.publicKey.x963Representation))
                : keys.identity
            let response = HTTPReply(status: 200, body: try JSONEncoder().encode(identity))
            responseBytes += response.body.count
            if heldHello.contains(host) { return await hold(response, event: .hello(host)) }
            observer?.yield(.hello(host))
            return response
        }
        try #require(url.path == "/direct/request" && request.httpMethod == "POST")
        let body = try #require(request.httpBody)
        requestBytes += body.count
        let envelope = try JSONDecoder().decode(Envelope.self, from: body)
        let clear = try envelope.open(senderKey: keys.device.publicKey, recipientKey: keys.macAgreement,
                                      sender: "phone-fixture", recipient: "mac-fixture", route: "mac-fixture", nowMilliseconds: time.milliseconds)
        let logical = try #require(JSONSerialization.jsonObject(with: clear) as? [String: Any])
        let method = try #require(logical["method"] as? String)
        if method != "GET" { effects += 1 }
        try #require(method == "GET" && logical["path"] as? String == "/views/card%3Acontrolled?protocol=1&client=mac&lang=en")
        reads += 1
        let response = try sealedResponse(requestID: envelope.requestID)
        responseBytes += response.body.count
        if heldResponses { return await hold(response, event: .request(host)) }
        observer?.yield(.request(host))
        return response
    }
    private func hold(_ response: HTTPReply, event: Event) async -> HTTPReply {
        await withCheckedContinuation {
            pending.append(($0, response, event))
            observer?.yield(event)
        }
    }
    private func sealedResponse(requestID: String) throws -> HTTPReply {
        if reply == .unsigned { return HTTPReply(status: 200, body: freshnessBody) }
        var answer: [String: Any] = ["status": reply.status, "body": try JSONSerialization.jsonObject(with: freshnessBody)]
        if reply == .revoked { answer["body"] = ["error": "device_revoked"] }
        if reply == .uncertain { answer["error"] = "uncertain" }
        let envelope = try Envelope.seal(route: "mac-fixture", sender: "mac-fixture", recipient: "phone-fixture",
            requestID: requestID, kind: "response", expiresAtMilliseconds: time.milliseconds + 120_000,
            plaintext: try JSONSerialization.data(withJSONObject: answer, options: [.sortedKeys]),
            senderKey: reply == .wrongKeys ? keys.impostor : keys.mac, recipientKey: keys.deviceAgreement.publicKey)
        var body = try JSONEncoder().encode(envelope)
        if reply == .unsupportedEnvelope {
            var json = try #require(JSONSerialization.jsonObject(with: body) as? [String: Any])
            json["v"] = 2
            body = try JSONSerialization.data(withJSONObject: json)
        }
        return HTTPReply(status: 200, body: body)
    }
}

private func freshnessResources() -> (cpu: Double, rss: Int) {
    var usage = rusage()
    precondition(getrusage(RUSAGE_SELF, &usage) == 0)
    let millis = { (time: timeval) in Double(time.tv_sec) * 1_000 + Double(time.tv_usec) / 1_000 }
    return (millis(usage.ru_utime) + millis(usage.ru_stime), Int(usage.ru_maxrss))
}

/// A Mac whose every address fails the way iOS fails a local address.
private struct NoPath: HTTPTransport {
    let error: URLError
    func send(_ request: URLRequest) async throws -> HTTPReply { throw error }
}

@Suite struct DirectLocalNetworkTests {
    private func failure(_ error: URLError, path reason: NWPath.UnsatisfiedReason) async throws -> DirectRouteError? {
        let http = DirectHTTP(transport: NoPath(error: error), localNetwork: LocalNetworkProbe { _ in reason })
        let router = DirectRouter(pinned: try FreshnessKeys().identity, saved: [freshnessLAN], http: http, network: { "home" })
        do { _ = try await router.endpoint() } catch let error as DirectRouteError { return error }
        return nil
    }

    @Test func aPathRefusedForLocalNetworkNamesThePermissionWhateverTheErrorSays() async throws {
        #expect(try await failure(URLError(.notConnectedToInternet), path: .localNetworkDenied) == .localNetworkDenied)
    }

    @Test func theOldPrivateDescriptionNoLongerDecidesThePermission() async throws {
        let described = URLError(.notConnectedToInternet,
            userInfo: ["_NSURLErrorNWPathKey": "unsatisfied (Local network prohibited), interface: en0[802.11]"])
        #expect(try await failure(described, path: .notAvailable) == .unreachable)
    }

    @Test func anotherFailureIsNeverAskedAboutThePermission() async throws {
        #expect(try await failure(URLError(.cannotConnectToHost), path: .localNetworkDenied) == .unreachable)
    }
}
