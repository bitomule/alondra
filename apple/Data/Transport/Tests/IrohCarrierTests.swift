@testable import AlondraTransport
import AlondraContract
import AlondraTestSupport
import CryptoKit
import Foundation
import Synchronization
import Testing

/// A Mac's iroh endpoint in memory: the first stream of a connection is the hello, every later
/// one is a request it runs once per request ID, unless the test scripts something else.
private actor FakeIrohMac {
    enum Reply: Sendable {
        /// The request reaches the Mac and the answer never comes back on this connection.
        case stall
        /// The Mac answers, after this long rendering it.
        case slow(Duration)
        case notSent, noAnswer, forged, revoked
        case error(String)
    }

    let macSigning = P256.Signing.PrivateKey()
    let macAgreement = P256.KeyAgreement.PrivateKey()
    let device = P256.Signing.PrivateKey()
    let deviceAgreement = P256.KeyAgreement.PrivateKey()
    let endpoint = IrohIdentity(seed: Data(repeating: 3, count: 32))
    var acceptsHello = true
    private(set) var hellos: [Data] = []
    private(set) var requests = 0
    private var script: [Reply] = []
    private var journal: [String: (request: Data, response: Data)] = [:]
    private var runs: [String: Int] = [:]

    func refuseHello() { acceptsHello = false }
    func acceptHello() { acceptsHello = true }
    func then(_ replies: Reply...) { script += replies }
    func executions(of requestID: String) -> Int { runs[requestID, default: 0] }

    func hello(_ body: Data) -> Data {
        hellos.append(body)
        return Data((acceptsHello ? #"{"ok":true}"# : #"{"error":"unauthorized"}"#).utf8)
    }

    func request(_ body: Data) async throws -> Data {
        requests += 1
        let envelope = try JSONDecoder().decode(Envelope.self, from: body)
        if !script.isEmpty {
            switch script.removeFirst() {
            case .stall: throw Stalled()
            case .slow(let delay):
                try await Task.sleep(for: delay)
                return try wrap("response", run(envelope, body: body))
            case .notSent: throw IrohLinkError.notSent
            case .noAnswer:
                _ = try run(envelope, body: body)
                throw IrohLinkError.noAnswer
            case .forged: return try wrap("response", seal(envelope.requestID, by: P256.Signing.PrivateKey()))
            case .revoked: return try wrap("revoked", seal(envelope.requestID, by: macSigning, revoked: true))
            case .error(let code):
                let refusal: [String: Any] = code == "rejected" ? ["error": code, "detail": "no"] : ["error": code]
                return try JSONSerialization.data(withJSONObject: refusal)
            }
        }
        return try wrap("response", run(envelope, body: body))
    }

    private func run(_ envelope: Envelope, body: Data) throws -> Data {
        if let stored = journal[envelope.requestID] {
            guard stored.request == body else { throw IrohLinkError.noAnswer }
            return stored.response
        }
        _ = try envelope.open(senderKey: device.publicKey, recipientKey: macAgreement, sender: "phone-1",
                              recipient: "mac-1", route: "mac-1", nowMilliseconds: nil)
        runs[envelope.requestID, default: 0] += 1
        let response = try seal(envelope.requestID, by: macSigning)
        journal[envelope.requestID] = (body, response)
        return response
    }

    private func wrap(_ key: String, _ sealed: Data) -> Data {
        Data("{\"\(key)\":".utf8) + sealed + Data("}".utf8)
    }

    private func seal(_ requestID: String, by signer: P256.Signing.PrivateKey, revoked: Bool = false) throws -> Data {
        let answer: [String: Any] = revoked
            ? ["status": 403, "error": "device_revoked", "body": ["error": "device_revoked"]]
            : ["status": 200, "body": ["ok": true]]
        return try JSONEncoder().encode(Envelope.seal(route: "mac-1", sender: "mac-1", recipient: "phone-1",
            requestID: requestID, kind: "response",
            expiresAtMilliseconds: UInt64(Date().timeIntervalSince1970 * 1000) + 60_000,
            plaintext: JSONSerialization.data(withJSONObject: answer), senderKey: signer,
            recipientKey: deviceAgreement.publicKey))
    }
}

private struct Stalled: Error {}

private final class FakeLink: IrohLink {
    private struct State {
        var helloSent = false
        var closed = false
        var path = IrohPath.relay
        var watchers: [AsyncStream<IrohPath>.Continuation] = []
    }

    private let mac: FakeIrohMac
    private let state = Mutex(State())

    init(mac: FakeIrohMac, path: IrohPath) {
        self.mac = mac
        state.withLock { $0.path = path }
    }

    func exchange(_ request: Data, answerLimit: Int) async throws -> Data {
        guard !isClosed else { throw IrohLinkError.notSent }
        let hello = state.withLock { state in
            defer { state.helloSent = true }
            return !state.helloSent
        }
        if hello { return await mac.hello(request) }
        do { return try await mac.request(request) }
        catch is Stalled {
            while !isClosed { try await Task.sleep(for: .milliseconds(5)) }
            throw IrohLinkError.noAnswer
        }
    }

    func path() -> IrohPath { state.withLock { $0.path } }

    func move(to path: IrohPath) {
        let watchers = state.withLock { state in
            state.path = path
            return state.watchers
        }
        for watcher in watchers { watcher.yield(path) }
    }

    func pathChanges() -> AsyncStream<IrohPath> {
        AsyncStream { continuation in
            let closed = state.withLock { state in
                if !state.closed { state.watchers.append(continuation) }
                return state.closed
            }
            if closed { continuation.finish() }
        }
    }

    var isClosed: Bool { state.withLock { $0.closed } }

    func close() async {
        let watchers = state.withLock { state in
            state.closed = true
            defer { state.watchers.removeAll() }
            return state.watchers
        }
        for watcher in watchers { watcher.finish() }
    }
}

private final class FakeDialer: IrohDialer {
    let endpointID = IrohIdentity(seed: Data(repeating: 4, count: 32)).endpointID
    let mac: FakeIrohMac
    private let links = Mutex<[FakeLink]>([])
    /// The path a connection made now gets: direct at home, through the relay on mobile data.
    private let network = Mutex(IrohPath.relay)
    private let moves = Mutex(0)
    private let delay = Mutex(Duration.zero)

    init(mac: FakeIrohMac) { self.mac = mac }

    var dials: Int { links.withLock { $0.count } }
    var last: FakeLink? { links.withLock { $0.last } }
    var rebinds: Int { moves.withLock { $0 } }
    func join(_ path: IrohPath) { network.withLock { $0 = path } }
    /// Connecting takes this long from now on, as it does on mobile data.
    func takes(_ duration: Duration) { delay.withLock { $0 = duration } }

    func dial(_ address: IrohMacAddress) async throws -> any IrohLink {
        try await Task.sleep(for: delay.withLock { $0 })
        let link = FakeLink(mac: mac, path: network.withLock { $0 })
        links.withLock { $0.append(link) }
        return link
    }

    func networkMoved() async { moves.withLock { $0 += 1 } }

    func shutdown() async {}
}

private final class Counter: Sendable {
    private let count = Mutex(0)
    var value: Int { count.withLock { $0 } }
    func increment() { count.withLock { $0 += 1 } }
}

private final class Events: Sendable {
    private let seen = Mutex<[IrohCarrier.Event]>([])
    var all: [IrohCarrier.Event] { seen.withLock { $0 } }
    func record(_ event: IrohCarrier.Event) { seen.withLock { $0.append(event) } }
}

private struct Fixture {
    let mac = FakeIrohMac()
    let dialer: FakeDialer
    let carrier: IrohCarrier
    let session: DirectSession
    let events = Events()
    let prepared = Counter()

    let journal: EnvelopeJournal?

    init(address: Bool = true, journal: EnvelopeJournal? = nil, quickSlack: Duration = .seconds(8),
         lingerAfterUse: Duration = .seconds(60), retryDelays: [Duration] = [.milliseconds(10)]) async throws {
        self.journal = journal
        dialer = FakeDialer(mac: mac)
        let macEndpoint = await mac.endpoint.endpointIDHex
        let relay = URL(string: "https://iroh.example.com")
        let prepared = prepared
        carrier = IrohCarrier(dialer: dialer, macID: "mac-1", deviceID: "phone-1", signing: mac.device,
            address: { address ? IrohMacAddress(endpointID: macEndpoint, relay: relay) : nil },
            prepare: { _ in prepared.increment() },
            connectWait: .seconds(5), lingerAfterUse: lingerAfterUse, retryDelays: retryDelays)
        session = DirectSession(macID: "mac-1", deviceID: "phone-1", signing: mac.device, agreement: mac.deviceAgreement,
            macSigning: mac.macSigning.publicKey, macAgreement: mac.macAgreement.publicKey, carrier: carrier,
            journal: journal, now: { UInt64(Date().timeIntervalSince1970 * 1000) }, requestClock: .continuous,
            quickSlack: quickSlack)
        let events = events
        await carrier.observe { events.record($0) }
    }

    func action(_ id: String) throws -> APIRequest {
        try APIRequest(endpoint: Endpoint<Data>.data(.post, path: ["actions"], body: Data(#"{"command":"a"}"#.utf8)),
                       operationID: id)
    }

    func read() throws -> APIRequest {
        try APIRequest(endpoint: Endpoint<Data>.data(.get, path: ["capabilities"]), operationID: "read")
    }
}

private let answered = APIResponse.completed(status: 200, body: Data(#"{"ok":true}"#.utf8))

struct IrohCarrierTests {
    @Test func requestsShareOneConnectionOpenedWithASignedHello() async throws {
        let f = try await Fixture()
        await f.carrier.start()
        #expect(try await f.session.perform(f.read()) == answered)
        #expect(try await f.session.perform(f.action("op-1")) == answered)
        #expect(try await f.session.perform(f.read()) == answered)
        #expect(f.dialer.dials == 1)
        #expect(f.prepared.value == 1, "the relay learns this endpoint before the first dial")
        let hello = try JSONDecoder().decode([String: String].self, from: try #require(await f.mac.hellos.first))
        let signature = try P256.Signing.ECDSASignature(rawRepresentation: Base64URL.decode(try #require(hello["signature"])))
        let signed = try IrohHello.bytes(macID: "mac-1", deviceID: "phone-1", deviceEndpoint: f.dialer.endpointID,
                                         macEndpoint: await f.mac.endpoint.endpointID)
        #expect(f.mac.device.publicKey.isValidSignature(signature, for: signed))
        #expect(await f.carrier.route == .irohRelay)
        guard case .connected(.irohRelay, _)? = f.events.all.first else {
            Issue.record("no connected event: \(f.events.all)"); return
        }
    }

    @Test func theRouteFollowsThePathIrohPicks() async throws {
        let f = try await Fixture()
        await f.carrier.start()
        _ = try await f.session.perform(f.read())
        try #require(f.dialer.last).move(to: .direct)
        try await waitUntil { await f.carrier.route == .direct }
        #expect(f.events.all.contains(.routeChanged(.direct)))
    }

    @Test func aRepeatedActionGetsTheMacsStoredAnswerAndRunsOnce() async throws {
        let f = try await Fixture()
        await f.carrier.start()
        let action = try f.action("op-1")
        #expect(try await f.session.perform(action) == answered)
        #expect(try await f.session.perform(action) == answered)
        #expect(await f.mac.executions(of: "op-1") == 1)
    }

    @Test func aStreamThatNeverLeftGoesAgainOnAFreshConnection() async throws {
        let f = try await Fixture()
        await f.carrier.start()
        _ = try await f.session.perform(f.read())
        await f.mac.then(.notSent)
        #expect(try await f.session.perform(f.action("op-1")) == answered)
        #expect(f.dialer.dials == 2)
        #expect(await f.mac.executions(of: "op-1") == 1)
    }

    @Test func anActionWithoutAnAnswerIsUnknownNeverDropped() async throws {
        let f = try await Fixture()
        await f.carrier.start()
        await f.mac.then(.noAnswer)
        #expect(try await f.session.perform(f.action("op-1")) == .unknown)
        #expect(await f.mac.executions(of: "op-1") == 1)
    }

    @Test func aReadWithoutAnAnswerTriesAgainThenGivesUp() async throws {
        let f = try await Fixture()
        await f.carrier.start()
        await f.mac.then(.noAnswer)
        #expect(try await f.session.perform(f.read()) == answered)
        await f.mac.then(.noAnswer, .noAnswer, .noAnswer, .noAnswer)
        await #expect(throws: IrohCarrierError.noAnswer) { try await f.session.quickRead(f.read()) }
    }

    @Test func onlyAnAnswerSealedWithThePinnedKeysCounts() async throws {
        let f = try await Fixture()
        await f.carrier.start()
        await f.mac.then(.forged, .forged)
        await #expect(throws: IrohCarrierError.noAnswer) { try await f.session.perform(f.read()) }
        await f.mac.then(.forged)
        #expect(try await f.session.perform(f.action("op-1")) == .unknown)
    }

    @Test func theMacsRefusalsKeepTheirMeaning() async throws {
        let f = try await Fixture()
        await f.carrier.start()
        await f.mac.then(.error("wrong_mac"))
        await #expect(throws: IrohCarrierError.refused("wrong_mac")) { try await f.session.perform(f.read()) }
        await f.mac.then(.error("idempotency_conflict"))
        await #expect(throws: SealedRequestError.operationIdentityConflict) { try await f.session.perform(f.action("op-1")) }
        await f.mac.then(.error("rejected"))
        guard case .completed(400, _) = try await f.session.perform(f.action("op-2")) else {
            Issue.record("a rejection is the Mac's answer"); return
        }
        await f.mac.then(.revoked)
        await #expect(throws: DirectSessionError.deviceRevoked) { try await f.session.perform(f.read()) }
    }

    @Test func aRefusedHelloSendsNothing() async throws {
        let f = try await Fixture()
        await f.mac.refuseHello()
        await f.carrier.start()
        await #expect(throws: IrohCarrierError.unavailable) { try await f.session.perform(f.action("op-1")) }
        #expect(await f.mac.requests == 0)
        #expect(await !f.carrier.isConnected)
    }

    @Test func withoutTheMacsEndpointNothingIsDialed() async throws {
        let f = try await Fixture(address: false)
        await f.carrier.start()
        await #expect(throws: IrohCarrierError.unavailable) { try await f.session.perform(f.read()) }
        #expect(f.dialer.dials == 0)
    }

    @Test func inTheBackgroundTheConnectionClosesAndTheForegroundOpensANewOne() async throws {
        let f = try await Fixture()
        await f.carrier.start()
        _ = try await f.session.perform(f.read())
        let first = try #require(f.dialer.last)
        await f.carrier.stop()
        #expect(first.isClosed)
        try await Task.sleep(for: .milliseconds(50))
        #expect(f.dialer.dials == 1, "nothing reconnects in the background on its own")
        await f.carrier.start()
        #expect(try await f.session.perform(f.read()) == answered)
        #expect(f.dialer.dials == 2)
    }

    /// 16 Oct: a notification button pressed with the iPhone locked must reach the Mac at once,
    /// with no mailbox to leave it in. The request opens its own connection.
    @Test func inTheBackgroundARequestOpensItsOwnConnection() async throws {
        let f = try await Fixture()
        #expect(try await f.session.perform(f.action("op-1")) == answered)
        #expect(f.dialer.dials == 1)
        #expect(await f.mac.executions(of: "op-1") == 1)
        guard case .connectedOnDemand(.irohRelay, _)? = f.events.all.first else {
            Issue.record("no on-demand connection measured: \(f.events.all)"); return
        }
    }

    @Test func backgroundRequestsShareTheConnectionOneOfThemOpened() async throws {
        let f = try await Fixture()
        f.dialer.takes(.milliseconds(100))
        let session = f.session
        let read = try f.read(), action = try f.action("op-1")
        async let first = session.perform(read)
        async let second = session.perform(action)
        #expect(try await first == answered)
        #expect(try await second == answered)
        #expect(try await session.perform(read) == answered)
        #expect(f.dialer.dials == 1)
    }

    @Test func aConnectionOpenedInTheBackgroundClosesOnceUnused() async throws {
        let f = try await Fixture(lingerAfterUse: .milliseconds(20))
        _ = try await f.session.perform(f.read())
        let link = try #require(f.dialer.last)
        try await waitUntil { link.isClosed }
        #expect(await !f.carrier.isConnected)
    }

    @Test func theForegroundKeepsTheConnectionABackgroundRequestOpened() async throws {
        let f = try await Fixture(lingerAfterUse: .milliseconds(20))
        _ = try await f.session.perform(f.read())
        await f.carrier.start()
        try await Task.sleep(for: .milliseconds(60))
        #expect(try await f.session.perform(f.read()) == answered)
        #expect(f.dialer.dials == 1)
        #expect(await f.carrier.isConnected)
    }

    @Test func aBackgroundRequestThatCannotConnectNeverLeavesTheDevice() async throws {
        let f = try await Fixture()
        await f.mac.refuseHello()
        await #expect(throws: IrohCarrierError.unavailable) { try await f.session.perform(f.action("op-1")) }
        #expect(await f.mac.requests == 0)
    }

    @Test func aShutDownCarrierOpensNothing() async throws {
        let f = try await Fixture()
        await f.carrier.shutdown()
        await #expect(throws: IrohCarrierError.unavailable) { try await f.session.perform(f.read()) }
        #expect(f.dialer.dials == 0)
    }

    /// Measured on mobile data: the connection takes 1 to 2.3 s after coming back. A read made
    /// meanwhile waits for it instead of failing.
    @Test func aRequestWaitsForTheConnectionBeingMade() async throws {
        let f = try await Fixture()
        f.dialer.takes(.milliseconds(300))
        await f.carrier.start()
        #expect(try await f.session.perform(f.read()) == answered)
        #expect(f.dialer.dials == 1)
    }

    @Test func aRequestDoesNotWaitOutTheBackoffOfAFailedConnection() async throws {
        let f = try await Fixture(retryDelays: [.seconds(3600)])
        await f.mac.refuseHello()
        await f.carrier.start()
        try await waitUntil { await f.mac.hellos.count == 1 }
        await f.mac.acceptHello()
        #expect(try await f.session.perform(f.read()) == answered)
        #expect(f.dialer.dials == 2)
    }

    @Test func aConnectionThatDropsIsReplaced() async throws {
        let f = try await Fixture()
        await f.carrier.start()
        _ = try await f.session.perform(f.read())
        await try #require(f.dialer.last).close()
        try await waitUntil { f.dialer.dials == 2 }
        #expect(try await f.session.perform(f.read()) == answered)
        #expect(f.events.all.contains(.lost))
    }

    /// 15 Oct, iPhone: from the home Wi-Fi to mobile data and back, the connection stayed on the
    /// relay. A move makes it again on the new network, so home is direct again within seconds.
    @Test func movingBetweenNetworksReconnectsOnEachOneBothWays() async throws {
        let f = try await Fixture()
        f.dialer.join(.direct)
        await f.carrier.start()
        #expect(try await f.session.perform(f.read()) == answered)
        #expect(await f.carrier.route == .direct)

        f.dialer.join(.relay)
        await f.carrier.networkMoved()
        #expect(try await f.session.perform(f.read()) == answered)
        #expect(await f.carrier.route == .irohRelay)

        f.dialer.join(.direct)
        await f.carrier.networkMoved()
        #expect(try await f.session.perform(f.read()) == answered)
        #expect(await f.carrier.route == .direct)
        #expect(f.dialer.dials == 3)
        #expect(f.dialer.rebinds == 2)
    }

    @Test func aReadCutOffByTheSwitchGoesAgainOnTheNewConnection() async throws {
        let f = try await Fixture()
        await f.carrier.start()
        _ = try await f.session.perform(f.read())
        await f.mac.then(.stall)
        let session = f.session
        let read = try f.read()
        let cutOff = Task { try await session.quickRead(read) }
        try await waitUntil { await f.mac.requests == 2 }
        await f.carrier.networkMoved()
        #expect(try await cutOff.value == answered)
        #expect(f.dialer.dials == 2)
    }

    @Test func aMoveInTheBackgroundOpensNothing() async throws {
        let f = try await Fixture()
        await f.carrier.networkMoved()
        #expect(f.dialer.rebinds == 0)
        #expect(f.dialer.dials == 0)
    }

    @Test func answersAreDecodedAsTheProtocolSaysOnly() throws {
        let open: (Envelope) throws -> APIResponse = { _ in answered }
        #expect(throws: IrohCarrierError.noAnswer) { try IrohCarrier.answer(Data("[]".utf8), open: open, mutation: false) }
        guard case .unknown = try IrohCarrier.answer(Data("{}".utf8), open: open, mutation: true) else {
            Issue.record("a mutation without a readable answer may have run"); return
        }
        #expect(throws: IrohCarrierError.refused("too_large")) {
            try IrohCarrier.answer(Data(#"{"error":"too_large"}"#.utf8), open: open, mutation: true)
        }
    }

    private func waitUntil(_ condition: @Sendable () async -> Bool) async throws {
        for _ in 0..<200 {
            if await condition() { return }
            try await Task.sleep(for: .milliseconds(10))
        }
        Issue.record("condition never held")
    }
}

@Suite(.temporaryFiles)
struct RoutedIrohTests {
    private func routed(_ f: Fixture) -> RoutedSession {
        RoutedSession(iroh: .init(session: f.session, carrier: f.carrier), direct: nil, router: nil)
    }

    @Test func aLiveConnectionTakesEveryRequest() async throws {
        let f = try await Fixture()
        let routed = routed(f)
        await routed.goLive()
        #expect(try await routed.perform(f.read()) == answered)
        #expect(try await routed.perform(f.action("op-1")) == answered)
        #expect(await routed.route == .irohRelay)
    }

    @Test func withoutTheMacsEndpointARequestFailsAtOnceWithoutDialing() async throws {
        let f = try await Fixture(address: false)
        let routed = routed(f)
        await routed.goLive()
        await #expect(throws: IrohCarrierError.unavailable) { try await routed.perform(f.read()) }
        #expect(f.dialer.dials == 0)
    }

    @Test func inTheBackgroundARequestStillReachesTheMac() async throws {
        let f = try await Fixture()
        let routed = routed(f)
        await routed.goLive()
        _ = try await routed.perform(f.read())
        await routed.goIdle()
        #expect(try await routed.perform(f.action("op-1")) == answered)
        #expect(await f.mac.executions(of: "op-1") == 1)
        #expect(f.dialer.dials == 2)
    }

    /// 15 Oct, iPhone at the office: a card the Mac took 9 s to render was asked again, so the
    /// Mac rendered it twice. While the connection lives, a slow Mac keeps the read.
    @Test func aSlowMacKeepsTheReadOnTheLiveConnection() async throws {
        let f = try await Fixture(quickSlack: .milliseconds(200))
        let routed = routed(f)
        await routed.goLive()
        await f.mac.then(.slow(.milliseconds(600)))
        #expect(try await routed.perform(f.read()) == answered)
        #expect(await f.mac.requests == 1)
        #expect(await routed.route == .irohRelay)
    }

    @Test func anActionThatMayHaveRunStaysUnknownAndNeverGoesTwice() async throws {
        let f = try await Fixture()
        let routed = routed(f)
        await routed.goLive()
        await f.mac.then(.noAnswer)
        #expect(try await routed.perform(f.action("op-1")) == .unknown)
        #expect(await f.mac.executions(of: "op-1") == 1)
    }

    @Test func anActionIrohKeptButCouldNotSendIsResentAsTheSameBytes() async throws {
        let scope = EnvelopeJournalScope(relay: URL(string: "https://relay.example.test")!, macID: "mac-1",
                                         deviceID: "phone-1", credentialRevision: "key")
        let journal = try EnvelopeJournal(scope: scope, directory: TestFiles.directory())
        let f = try await Fixture(journal: journal)
        let routed = routed(f)
        await routed.goLive()
        _ = try await routed.perform(f.read())
        await f.mac.then(.notSent, .notSent)
        let action = try f.action("op-1")
        await #expect(throws: IrohCarrierError.unavailable) { try await routed.perform(action) }
        let kept = try #require(try await journal.entry(requestID: "op-1")?.envelope)
        #expect(await f.mac.executions(of: "op-1") == 0)
        #expect(try await routed.resend(action) == answered)
        #expect(await f.mac.executions(of: "op-1") == 1)
        #expect(try await journal.entry(requestID: "op-1")?.envelope == kept, "never sealed again")
    }

    @Test func aResentActionGoesOverIrohAsTheSameBytes() async throws {
        let scope = EnvelopeJournalScope(relay: URL(string: "https://relay.example.test")!, macID: "mac-1",
                                         deviceID: "phone-1", credentialRevision: "key")
        let journal = try EnvelopeJournal(scope: scope, directory: TestFiles.directory())
        let f = try await Fixture(journal: journal)
        let routed = routed(f)
        await routed.goLive()
        await f.mac.then(.noAnswer)
        let action = try f.action("op-1")
        #expect(try await routed.perform(action) == .unknown)
        #expect(try await routed.resend(action) == answered)
        #expect(await f.mac.executions(of: "op-1") == 1)
    }
}
