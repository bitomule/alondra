@testable import AlondraTransport
import AlondraContract
import CryptoKit
import Foundation
import Testing

/// A Mac's direct port in memory: answers `hello` per address and runs each request ID once.
/// Hosts in `forged` answer like an impostor that copied the Mac's public keys: a 200 with an
/// envelope it could not have signed.
private actor FakeDirectMac: HTTPTransport {
    let macSigning = P256.Signing.PrivateKey()
    let macAgreement = P256.KeyAgreement.PrivateKey()
    let device = P256.Signing.PrivateKey()
    let deviceAgreement = P256.KeyAgreement.PrivateKey()
    var helloByHost: [String: DirectMacIdentity] = [:]
    var forged: Set<String> = []
    var statusByHost: [String: Int] = [:]
    var failNextRequests = 0
    private(set) var requestBodies: [Data] = []
    private(set) var hosts: [String] = []
    private var journal: [String: (request: Data, response: Data)] = [:]
    private var executions = 0
    private var runs: [String: Int] = [:]

    var identity: DirectMacIdentity {
        DirectMacIdentity(macID: "mac-1", name: "Studio",
                          signingKey: Base64URL.encode(macSigning.publicKey.x963Representation),
                          agreementKey: Base64URL.encode(macAgreement.publicKey.x963Representation))
    }

    var impostor: DirectMacIdentity {
        DirectMacIdentity(macID: "mac-1", name: "Studio",
                          signingKey: Base64URL.encode(P256.Signing.PrivateKey().publicKey.x963Representation),
                          agreementKey: Base64URL.encode(P256.KeyAgreement.PrivateKey().publicKey.x963Representation))
    }

    func answerHello(on host: String, with identity: DirectMacIdentity?) { helloByHost[host] = identity }
    func forge(on host: String) { forged.insert(host) }
    func answer(on host: String, with status: Int) { statusByHost[host] = status }
    func failRequests(_ count: Int) { failNextRequests = count }
    var executed: Int { executions }
    func executions(of requestID: String) -> Int { runs[requestID, default: 0] }

    func send(_ request: URLRequest) async throws -> HTTPReply {
        let host = request.url!.host()!
        if request.url!.path == "/direct/hello" {
            guard let identity = helloByHost[host] else { throw URLError(.cannotConnectToHost) }
            return HTTPReply(status: 200, body: try JSONEncoder().encode(identity))
        }
        hosts.append(host)
        requestBodies.append(request.httpBody!)
        if failNextRequests > 0 {
            failNextRequests -= 1
            throw URLError(.networkConnectionLost)
        }
        if let status = statusByHost[host] { return HTTPReply(status: status, body: Data("{}".utf8)) }
        let envelope = try JSONDecoder().decode(Envelope.self, from: request.httpBody!)
        if forged.contains(host) { return HTTPReply(status: 200, body: try seal(envelope.requestID, by: P256.Signing.PrivateKey())) }
        guard let body = try run(envelope) else { return HTTPReply(status: 409, body: Data(#"{"error":"idempotency_conflict"}"#.utf8)) }
        return HTTPReply(status: 200, body: body)
    }

    /// What the relay hands the Mac: the same journal, whichever carrier brought the envelope.
    func deliverThroughTheRelay(_ envelope: Envelope) throws { _ = try run(envelope) }

    /// Runs a request ID once; the same bytes again get the stored answer, other bytes nothing.
    private func run(_ envelope: Envelope) throws -> Data? {
        let encoder = JSONEncoder()
        encoder.outputFormatting = .sortedKeys
        let request = try encoder.encode(envelope)
        if let stored = journal[envelope.requestID] { return stored.request == request ? stored.response : nil }
        _ = try envelope.open(senderKey: device.publicKey, recipientKey: macAgreement, sender: "phone-1",
                              recipient: "mac-1", route: "mac-1", nowMilliseconds: nil)
        executions += 1
        runs[envelope.requestID, default: 0] += 1
        let body = try seal(envelope.requestID, by: macSigning)
        journal[envelope.requestID] = (request, body)
        return body
    }

    private func seal(_ requestID: String, by signer: P256.Signing.PrivateKey) throws -> Data {
        let plain = try JSONSerialization.data(withJSONObject: ["status": 200, "body": ["ok": true]])
        return try JSONEncoder().encode(Envelope.seal(route: "mac-1", sender: "mac-1", recipient: "phone-1",
            requestID: requestID, kind: "response",
            expiresAtMilliseconds: UInt64(Date().timeIntervalSince1970 * 1000) + 60_000,
            plaintext: plain, senderKey: signer, recipientKey: deviceAgreement.publicKey))
    }
}

private let lan = DirectEndpoint(host: "192.168.1.5", port: 47810, kind: .localNetwork)
private let tailnetIP = DirectEndpoint(host: "100.100.1.1", port: 47810, kind: .tailscale)
private let tailnetName = DirectEndpoint(host: "studio.tail1.ts.net", port: 47810, kind: .tailscale)

private final class CurrentNetwork: @unchecked Sendable {
    private let lock = NSLock()
    private var name = "home"
    var value: String { lock.withLock { name } }
    func join(_ network: String) { lock.withLock { name = network } }
}

private struct Fixture {
    let mac = FakeDirectMac()
    let network = CurrentNetwork()
    let router: DirectRouter
    let session: DirectSession

    /// `saved` are addresses a sealed answer proved before; `offered` came from the Mac's routes.
    init(saved: [DirectEndpoint] = [lan], offered: [DirectEndpoint] = [], answering: [DirectEndpoint]? = nil) async throws {
        let http = DirectHTTP(transport: mac)
        let network = network
        router = DirectRouter(pinned: await mac.identity, saved: saved, http: http, network: { network.value })
        await router.offer(offered, from: .mac)
        session = DirectSession(macID: "mac-1", deviceID: "phone-1", signing: mac.device,
            agreement: mac.deviceAgreement, macSigning: mac.macSigning.publicKey,
            macAgreement: mac.macAgreement.publicKey, router: router, http: http, pause: { _ in })
        for endpoint in answering ?? (saved + offered) {
            await mac.answerHello(on: endpoint.host, with: await mac.identity)
        }
    }

    func action(_ id: String, body: String = #"{"command":"a"}"#) throws -> APIRequest {
        try APIRequest(endpoint: Endpoint<Data>.data(.post, path: ["actions"], body: Data(body.utf8)), operationID: id)
    }

    func read() throws -> APIRequest {
        try APIRequest(endpoint: Endpoint<Data>.data(.get, path: ["capabilities"]), operationID: "read")
    }
}

private let answered = APIResponse.completed(status: 200, body: Data(#"{"ok":true}"#.utf8))

@Suite struct DirectSessionTests {
    @Test func aLostAnswerIsResentAsTheSameBytesAndRunsOnce() async throws {
        let f = try await Fixture()
        await f.mac.failRequests(1)
        #expect(try await f.session.perform(f.action("op-1")) == answered)
        let bodies = await f.mac.requestBodies
        #expect(bodies.count == 2)
        #expect(bodies[0] == bodies[1])
        #expect(await f.mac.executed == 1)
    }

    @Test func retryingAnOperationResendsItsFirstEnvelope() async throws {
        let f = try await Fixture()
        _ = try await f.session.perform(f.action("op-1"))
        _ = try await f.session.perform(f.action("op-1"))
        let bodies = await f.mac.requestBodies
        #expect(bodies[0] == bodies[1])
        #expect(await f.mac.executed == 1)
    }

    @Test func anOperationIDWithOtherContentIsRefused() async throws {
        let f = try await Fixture()
        _ = try await f.session.perform(f.action("op-1"))
        await #expect(throws: RelaySessionError.operationIdentityConflict) {
            try await f.session.perform(f.action("op-1", body: #"{"command":"b"}"#))
        }
    }

    @Test func aMutationWithNoAnswerIsUnknownNotLost() async throws {
        let f = try await Fixture()
        await f.mac.failRequests(4)
        #expect(try await f.session.perform(f.action("op-1")) == .unknown)
    }

    @Test func anUnsignedRefusalNeverMarksTheDeviceRevoked() async throws {
        let f = try await Fixture()
        _ = try await f.session.perform(f.read())
        await f.mac.answer(on: lan.host, with: 401)
        await #expect(throws: DirectRouteError.unreachable) { try await f.session.perform(f.action("op-1")) }
        #expect(await !f.router.isProven(lan))
    }

    @Test func anAddressProvedOnThisNetworkReportingAnIdempotencyConflictIsAnIdentityConflict() async throws {
        let f = try await Fixture()
        _ = try await f.session.perform(f.read())
        await f.mac.answer(on: lan.host, with: 409)
        await #expect(throws: RelaySessionError.operationIdentityConflict) { try await f.session.perform(f.action("op-1")) }
    }

    @Test func anUnprovedAddressCannotClaimTheDeviceIsRevoked() async throws {
        let f = try await Fixture(saved: [], offered: [lan, tailnetIP])
        await f.mac.answer(on: lan.host, with: 401)
        #expect(try await f.session.perform(f.read()) == answered)
        #expect(await f.mac.hosts == [lan.host, tailnetIP.host])
    }

    @Test func anImpostorWithTheMacsPublicKeysIsSkippedAndNeverSaved() async throws {
        let f = try await Fixture(saved: [], offered: [lan, tailnetIP])
        await f.mac.forge(on: lan.host)
        #expect(try await f.session.perform(f.read()) == answered)
        #expect(await f.router.provenEndpoints == [tailnetIP])
        #expect(await f.router.route == .tailscale)
    }

    @Test func operationsAgeOutOfTheResendBuffer() async throws {
        let f = try await Fixture()
        _ = try await f.session.perform(f.action("op-0"))
        await #expect(throws: RelaySessionError.operationIdentityConflict) {
            try await f.session.perform(f.action("op-0", body: #"{"command":"later"}"#))
        }
        for index in 1...DirectSession.maxSealed { _ = try await f.session.perform(f.action("op-\(index)")) }
        await #expect(throws: RelaySessionError.operationIdentityConflict) {
            try await f.session.perform(f.action("op-0", body: #"{"command":"later"}"#))
        }
        #expect(await f.mac.executions(of: "op-0") == 1)
    }
}

@Suite struct DirectRouterTests {
    @Test func prefersTheLANThenTheTailnetIPThenItsName() async throws {
        let f = try await Fixture(saved: [], offered: [tailnetName, tailnetIP, lan])
        #expect(await f.router.known == [lan, tailnetIP, tailnetName])
        #expect(try await f.router.endpoint() == lan)
        #expect(await f.router.route == .localNetwork)
    }

    @Test func failsOverToTheTailnetWhenTheLANStopsAnswering() async throws {
        let f = try await Fixture(saved: [lan, tailnetName])
        #expect(try await f.router.endpoint() == lan)
        await f.mac.answerHello(on: lan.host, with: nil)
        await f.router.invalidate()
        #expect(try await f.router.endpoint() == tailnetName)
        #expect(await f.router.route == .tailscale)
    }

    @Test func anotherMacAtAKnownAddressIsSkipped() async throws {
        let f = try await Fixture(saved: [lan])
        let stranger = DirectMacIdentity(macID: "mac-2", name: "Other",
            signingKey: Base64URL.encode(P256.Signing.PrivateKey().publicKey.x963Representation),
            agreementKey: Base64URL.encode(P256.KeyAgreement.PrivateKey().publicKey.x963Representation))
        await f.mac.answerHello(on: lan.host, with: stranger)
        await #expect(throws: DirectRouteError.unreachable) { try await f.router.endpoint() }
    }

    @Test func anImpostorBesideTheRealMacChangesNothing() async throws {
        let f = try await Fixture(saved: [lan, tailnetIP])
        await f.mac.answerHello(on: lan.host, with: await f.mac.impostor)
        #expect(try await f.router.endpoint() == tailnetIP)
        #expect(try await f.session.perform(f.read()) == answered)
    }

    @Test func anImpostorAloneLeavesTheMacAsNotAnsweringAndIsNeverRemembered() async throws {
        let f = try await Fixture(saved: [lan])
        await f.mac.answerHello(on: lan.host, with: await f.mac.impostor)
        await #expect(throws: DirectRouteError.unreachable) { try await f.router.endpoint() }
        await f.mac.answerHello(on: lan.host, with: nil)
        await f.mac.answerHello(on: tailnetIP.host, with: await f.mac.identity)
        await f.router.offer([tailnetIP], from: .mac)
        #expect(try await f.router.endpoint() == tailnetIP)
    }

    @Test func aSavedAddressIsProvedAgainOnANewNetworkBeforeItsAnswerIsBelieved() async throws {
        let f = try await Fixture()
        #expect(try await f.session.perform(f.read()) == answered)
        #expect(await f.router.isProven(lan))
        f.network.join("café")
        #expect(await !f.router.isProven(lan))
        await f.mac.answer(on: lan.host, with: 409)
        await #expect(throws: DirectRouteError.unreachable) { try await f.session.perform(f.action("op-1")) }
        #expect(await f.router.provenEndpoints == [lan])
    }

    @Test func noAnsweringAddressMeansUnreachable() async throws {
        let f = try await Fixture(saved: [tailnetIP], answering: [])
        await #expect(throws: DirectRouteError.unreachable) { try await f.session.perform(f.action("op-1")) }
        #expect(await f.mac.requestBodies.isEmpty)
    }

    @Test func aBonjourFloodNeverPushesOutSavedOrMacGivenAddresses() async throws {
        let f = try await Fixture(saved: [lan], offered: [tailnetIP])
        let flood = (1...50).map { DirectEndpoint(host: "10.0.0.\($0)", port: 47810, kind: .localNetwork) }
        await f.router.offer(flood, from: .bonjour)
        let known = await f.router.known
        #expect(known.contains(lan))
        #expect(known.contains(tailnetIP))
        #expect(known.count == 2 + DirectRouter.maxBonjour)
        #expect(await f.router.provenEndpoints == [lan])
    }

    @Test func routesFromTheMacBecomeEndpoints() throws {
        let reply = try JSONDecoder().decode(DirectRoutesReply.self, from: Data(
            #"{"port":47810,"lan":["192.168.1.5"],"tailscale":{"ip":"100.100.1.1","name":"studio.tail1.ts.net."}}"#.utf8))
        #expect(reply.endpoints == [lan, tailnetIP, tailnetName])
    }
}

/// The relay as `RoutedSession` sees it: answers, or fails before sending anything.
private actor FakeRelay: RelayOperations {
    private(set) var calls = 0
    var failing = false
    private var sent: [String: Envelope] = [:]
    private var losesAnswersTo: (FakeDirectMac, EnvelopeCodec)?

    func fail(_ value: Bool) { failing = value }

    /// Hands every mutation to the Mac and then loses the Mac's answer on the way back.
    func loseAnswers(from mac: FakeDirectMac, sealingWith codec: EnvelopeCodec) { losesAnswersTo = (mac, codec) }

    func perform(_ request: APIRequest) async throws -> APIResponse {
        calls += 1
        if failing { throw URLError(.notConnectedToInternet) }
        if let (mac, codec) = losesAnswersTo, request.method == "POST" {
            let envelope = try codec.sealRequest(requestID: request.operationID, method: request.method,
                                                 path: request.path, body: request.body)
            sent[request.operationID] = envelope
            try await mac.deliverThroughTheRelay(envelope)
            return .unknown
        }
        return .completed(status: 200, body: Data(#"{"relay":true}"#.utf8))
    }
    func sentEnvelope(for requestID: String) async -> Envelope? { sent[requestID] }
    func registerServicePush(token: String, environment: String, binding: String) async throws {}
    func deactivate() async {}
}

private let viaRelay = APIResponse.completed(status: 200, body: Data(#"{"relay":true}"#.utf8))

@Suite struct RoutedSessionTests {
    @Test func anImpostorOnTheNetworkNeverBlocksTheRelay() async throws {
        let f = try await Fixture(saved: [lan])
        await f.mac.answerHello(on: lan.host, with: await f.mac.impostor)
        let relay = FakeRelay()
        await relay.fail(true)
        let routed = RoutedSession(direct: f.session, router: f.router, relay: relay)
        await #expect(throws: URLError.self) { try await routed.perform(f.read()) }
        await relay.fail(false)
        #expect(try await routed.perform(f.read()) == viaRelay)
        #expect(try await routed.perform(f.read()) == viaRelay)
    }

    @Test func aChangeTheRelaySentAndTheDirectPathResendsRunsOnceOnTheMac() async throws {
        let f = try await Fixture()
        let relay = FakeRelay()
        let codec = EnvelopeCodec(macID: "mac-1", deviceID: "phone-1", signing: f.mac.device, agreement: f.mac.deviceAgreement,
                                  macSigning: f.mac.macSigning.publicKey, macAgreement: f.mac.macAgreement.publicKey,
                                  now: { UInt64(Date().timeIntervalSince1970 * 1000) })
        await relay.loseAnswers(from: f.mac, sealingWith: codec)
        let routed = RoutedSession(direct: f.session, router: f.router, relay: relay, probeInterval: .seconds(3600))
        #expect(try await routed.perform(f.action("op-1")) == answered)
        #expect(await f.mac.executions(of: "op-1") == 1)
        #expect(await routed.route == .localNetwork)
    }

    @Test func withARelayNoRequestWaitsOnTheDirectPath() async throws {
        let f = try await Fixture(saved: [lan])
        let relay = FakeRelay()
        let routed = RoutedSession(direct: f.session, router: f.router, relay: relay)
        #expect(try await routed.perform(f.read()) == viaRelay)
        #expect(await routed.route == .relay)
        for _ in 0..<100 where await f.mac.hosts.isEmpty { try await Task.sleep(for: .milliseconds(10)) }
        #expect(await f.mac.hosts == [lan.host])
        #expect(try await routed.perform(f.read()) == answered)
        #expect(await routed.route == .localNetwork)
        #expect(await relay.calls == 1)
    }

    @Test func withoutInternetTheMacAnswersDirectly() async throws {
        let f = try await Fixture(saved: [lan])
        let relay = FakeRelay()
        await relay.fail(true)
        let routed = RoutedSession(direct: f.session, router: f.router, relay: relay, probeInterval: .seconds(3600))
        #expect(try await routed.perform(f.read()) == answered)
    }

    @Test func aRelayPairingStaysOffTheLocalNetworkUntilAllowed() async throws {
        let f = try await Fixture(saved: [lan])
        let relay = FakeRelay()
        await relay.fail(true)
        let routed = RoutedSession(direct: f.session, router: f.router, relay: relay, directAllowed: { false })
        await #expect(throws: URLError.self) { try await routed.perform(f.read()) }
        try await Task.sleep(for: .milliseconds(50))
        #expect(await f.mac.hosts.isEmpty)
    }

    @Test func anUnsignedRefusalOnTheDirectPathFallsBackToTheRelay() async throws {
        let f = try await Fixture(saved: [lan])
        let relay = FakeRelay()
        let routed = RoutedSession(direct: f.session, router: f.router, relay: relay)
        _ = try await routed.perform(f.read())
        for _ in 0..<100 where await routed.route != .localNetwork {
            _ = try? await routed.perform(f.read())
            try await Task.sleep(for: .milliseconds(10))
        }
        await f.mac.answer(on: lan.host, with: 401)
        await f.router.reject(lan)
        #expect(try await routed.perform(f.read()) == viaRelay)
    }
}
