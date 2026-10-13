@testable import AlondraTransport
import AlondraContract
import AlondraTestSupport
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
    var revokedAnswers: Set<String> = []
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
    func sayRevoked(on host: String) { revokedAnswers.insert(host) }
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
        let revoked = revokedAnswers.contains(host)
        if forged.contains(host) {
            return HTTPReply(status: 200, body: try seal(envelope.requestID, by: P256.Signing.PrivateKey(), revoked: revoked))
        }
        if revoked { return HTTPReply(status: 200, body: try seal(envelope.requestID, by: macSigning, revoked: true)) }
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

    private func seal(_ requestID: String, by signer: P256.Signing.PrivateKey, revoked: Bool = false) throws -> Data {
        let answer: [String: Any] = revoked
            ? ["status": 403, "error": "device_revoked", "body": ["error": "device_revoked"]]
            : ["status": 200, "body": ["ok": true]]
        let plain = try JSONSerialization.data(withJSONObject: answer)
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

@Suite(.temporaryFiles) struct DirectSessionTests {
    @Test func durableActionsDoNotInterceptLiveSettingsRetries() async throws {
        let fixture = try await Fixture()
        let scope = RelayOutboxScope(relay: URL(string: "https://relay.fixture.invalid")!, macID: "mac-1",
            deviceID: "phone-1", credentialRevision: "approved")
        let directory = try TestFiles.directory()
        let session = DirectSession(macID: "mac-1", deviceID: "phone-1", signing: fixture.mac.device,
            agreement: fixture.mac.deviceAgreement, macSigning: fixture.mac.macSigning.publicKey,
            macAgreement: fixture.mac.macAgreement.publicKey, router: fixture.router,
            http: DirectHTTP(transport: fixture.mac),
            journal: try EnvelopeJournal(scope: scope, directory: directory), pause: { _ in })
        let request = try APIRequest(endpoint: Endpoint<Data>.data(.put, path: ["settings"],
            body: Data(#"{"projects_root":"/fixture"}"#.utf8)), operationID: "settings-original")
        #expect(try await session.perform(request) == answered)
        #expect(try await session.perform(request) == answered)
        #expect(await fixture.mac.executions(of: request.operationID) == 1)
        let bodies = await fixture.mac.requestBodies
        #expect(bodies.count == 2)
        #expect(bodies.first == bodies.last)
        #expect(try FileManager.default.contentsOfDirectory(atPath: directory.path).isEmpty, "settings does not consult the action journal")
        await session.deactivate()
    }

    @Test func aLostAnswerIsResentAsTheSameBytesAndRunsOnce() async throws {
        let f = try await Fixture()
        await f.mac.failRequests(1)
        #expect(try await f.session.perform(f.action("op-1")) == answered)
        let bodies = await f.mac.requestBodies
        #expect(bodies.count == 2)
        #expect(bodies[0] == bodies[1])
        #expect(await f.mac.executed == 1)
    }

    @Test func aFolderChangeTravelsAsAMutationAndRunsOnce() async throws {
        let f = try await Fixture()
        await f.mac.failRequests(1)
        let change = try APIRequest(endpoint: Endpoint<Data>.data(.put, path: ["settings"],
                                                                 body: Data(#"{"projects_root":"/Users/d/Code"}"#.utf8)),
                                    operationID: "folder-1")
        #expect(try await f.session.perform(change) == answered)
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

    @Test func anUnsealedStatusNeverBecomesAnExpiredSessionOrALogin() async throws {
        for status in [403, 404, 500, 502] {
            let proved = try await Fixture()
            _ = try await proved.session.perform(proved.read())
            await proved.mac.answer(on: lan.host, with: status)
            await #expect(throws: DirectRouteError.unreachable, "\(status) from a proved address") {
                try await proved.session.perform(proved.action("op-\(status)"))
            }

            let saved = try await Fixture(saved: [], offered: [lan])
            await saved.mac.answer(on: lan.host, with: status)
            await #expect(throws: DirectRouteError.unreachable, "\(status) from an address never proved") {
                try await saved.session.perform(saved.read())
            }
        }
    }

    @Test func aSealedRevocationFromTheMacIsBelievedAndProvesTheAddress() async throws {
        let f = try await Fixture()
        await f.mac.sayRevoked(on: lan.host)
        await #expect(throws: DirectSessionError.deviceRevoked) { try await f.session.perform(f.read()) }
        await #expect(throws: DirectSessionError.deviceRevoked) { try await f.session.perform(f.action("op-1")) }
        #expect(await f.router.isProven(lan))
    }

    @Test func aRevocationSealedByAnImpostorIsNotBelieved() async throws {
        let f = try await Fixture()
        await f.mac.sayRevoked(on: lan.host)
        await f.mac.forge(on: lan.host)
        await #expect(throws: DirectRouteError.unreachable) { try await f.session.perform(f.read()) }
        #expect(await !f.router.isProven(lan))
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

    @Test func anAddressProvedHereNowAnsweringWithOtherKeysOnlyLeavesAHint() async throws {
        let f = try await Fixture()
        #expect(try await f.session.perform(f.read()) == answered)
        await f.router.invalidate()
        await f.mac.answerHello(on: lan.host, with: await f.mac.impostor)
        await #expect(throws: DirectRouteError.identityChanged) { try await f.session.perform(f.read()) }
        await #expect(throws: DirectRouteError.identityChanged) { try await f.session.perform(f.action("op-1")) }
        #expect(await f.router.route == nil)
        #expect(await f.router.provenEndpoints == [lan])
        #expect(await f.mac.hosts == [lan.host], "nothing was sent to the impostor")
    }

    @Test func theHintNeedsAnAddressProvedOnThisNetworkAndGoesWhenTheRealMacAnswers() async throws {
        let unproved = try await Fixture()
        await unproved.mac.answerHello(on: lan.host, with: await unproved.mac.impostor)
        await #expect(throws: DirectRouteError.unreachable) { try await unproved.router.endpoint() }

        let moved = try await Fixture()
        #expect(try await moved.session.perform(moved.read()) == answered)
        await moved.router.invalidate()
        await moved.mac.answerHello(on: lan.host, with: await moved.mac.impostor)
        moved.network.join("café")
        await #expect(throws: DirectRouteError.unreachable) { try await moved.router.endpoint() }

        let recovered = try await Fixture(saved: [lan, tailnetIP])
        #expect(try await recovered.session.perform(recovered.read()) == answered)
        await recovered.router.invalidate()
        await recovered.mac.answerHello(on: lan.host, with: await recovered.mac.impostor)
        await recovered.mac.answerHello(on: tailnetIP.host, with: await recovered.mac.identity)
        #expect(try await recovered.router.endpoint() == tailnetIP)
        await recovered.mac.answerHello(on: tailnetIP.host, with: nil)
        await recovered.router.invalidate()
        await #expect(throws: DirectRouteError.unreachable) { try await recovered.router.endpoint() }
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
        await routed.drainProbe()
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

extension DirectRouterTests {
    @Test(arguments: [false, true]) func anAddressRejectedOnAnotherNetworkIsProbedAgainWithoutTrustingHello(invalidate: Bool) async throws {
        let f = try await Fixture()
        #expect(try await f.session.perform(f.read()) == answered)
        await f.router.invalidate()
        await f.mac.answerHello(on: lan.host, with: await f.mac.impostor)
        await #expect(throws: DirectRouteError.identityChanged) { try await f.router.endpoint() }
        f.network.join("new-network")
        await f.mac.answerHello(on: lan.host, with: await f.mac.identity)
        if invalidate { await f.router.invalidate() }
        #expect(try await f.router.endpoint() == lan)
        #expect(await !f.router.isProven(lan), "the old proof belongs to the old network")
        #expect(try await f.session.perform(f.read()) == answered)
        #expect(await f.router.isProven(lan), "only the sealed answer proves the address on this network")
    }
}

extension DirectRouterTests {
    @Test func invalidationOnTheSameNetworkDoesNotForgetARejectedAddress() async throws {
        let f = try await Fixture()
        #expect(try await f.session.perform(f.read()) == answered)
        await f.router.invalidate()
        await f.mac.answerHello(on: lan.host, with: await f.mac.impostor)
        await #expect(throws: DirectRouteError.identityChanged) { try await f.router.endpoint() }
        await f.mac.answerHello(on: lan.host, with: await f.mac.identity)
        await f.router.invalidate()
        await #expect(throws: DirectRouteError.identityChanged) { try await f.router.endpoint() }
        #expect(await f.router.route == nil)
    }
}


private actor HeldPostedReply: HTTPTransport {
    private let mac: FakeDirectMac
    private let firstReply: HTTPReply?
    private let entered: AsyncStream<Void>
    private let signal: AsyncStream<Void>.Continuation
    private var hold = true
    private var pending: (HTTPReply, CheckedContinuation<HTTPReply, any Error>)?

    init(mac: FakeDirectMac, firstReply: HTTPReply? = nil) {
        self.mac = mac
        self.firstReply = firstReply
        let pair = AsyncStream<Void>.makeStream()
        entered = pair.stream; signal = pair.continuation
    }

    func starts() -> AsyncStream<Void> { entered }

    func send(_ request: URLRequest) async throws -> HTTPReply {
        let reply = try await mac.send(request)
        guard request.url?.path == "/direct/request", hold else { return reply }
        hold = false
        return try await withCheckedThrowingContinuation { continuation in
            pending = (firstReply ?? reply, continuation)
            signal.yield(())
        }
    }

    func release() {
        let completion = pending
        pending = nil
        if let completion { completion.1.resume(returning: completion.0) }
    }
}

extension DirectSessionTests {
    @Test func aSealedReplyFromThePreviousNetworkCannotProveTheCurrentAddress() async throws {
        let f = try await Fixture()
        let held = HeldPostedReply(mac: f.mac)
        let session = DirectSession(macID: "mac-1", deviceID: "phone-1", signing: f.mac.device,
            agreement: f.mac.deviceAgreement, macSigning: f.mac.macSigning.publicKey,
            macAgreement: f.mac.macAgreement.publicKey, router: f.router,
            http: DirectHTTP(transport: held), pause: { _ in })
        var starts = await held.starts().makeAsyncIterator()
        let pending = Task { try await session.perform(f.read()) }
        #expect(await starts.next() != nil)
        f.network.join("new-network")
        await f.router.invalidate()
        await held.release()
        #expect(try await pending.value == answered, "the authenticated response remains the same operation result")
        #expect(await !f.router.isProven(lan), "an old-network response cannot authenticate an address on the new network")
        #expect(try await session.perform(f.read()) == answered)
        #expect(await f.router.isProven(lan), "a fresh sealed request on the current network can prove it")
    }
}


extension DirectSessionTests {
    @Test(arguments: [200, 401, 409, 421, 400, 429, 503, 500], [0, 1, 2])
    func aLateUnsignedReplyCannotUseOrRejectAReplacementRoute(status: Int, transition: Int) async throws {
        let f = try await Fixture()
        #expect(try await f.session.perform(f.read()) == answered)
        let held = HeldPostedReply(mac: f.mac, firstReply: HTTPReply(status: status, body: Data("{}".utf8)))
        let session = DirectSession(macID: "mac-1", deviceID: "phone-1", signing: f.mac.device,
            agreement: f.mac.deviceAgreement, macSigning: f.mac.macSigning.publicKey,
            macAgreement: f.mac.macAgreement.publicKey, router: f.router,
            http: DirectHTTP(transport: held), pause: { _ in })
        var starts = await held.starts().makeAsyncIterator()
        let request = try f.action("held-unsigned")
        let pending = Task { () -> Result<APIResponse, any Error> in
            do { return .success(try await session.perform(request)) }
            catch { return .failure(error) }
        }
        #expect(await starts.next() != nil)
        if transition != 2 { f.network.join("new-network") }
        if transition != 0 { await f.router.invalidate() }
        #expect(try await f.session.perform(f.read()) == answered)
        #expect(await f.router.isProven(lan))
        await held.release()
        let result = await pending.value
        let response = try? result.get()
        #expect(response == answered, "a superseded unsigned status cannot become the operation result: \(result)")
        #expect(await f.router.isProven(lan), "the late unsigned reply cannot remove the replacement route's proof")
        #expect(await f.router.route == .localNetwork)
        #expect(await f.mac.executions(of: request.operationID) == 1, "the original applied action must not execute twice")
        let bodies = await f.mac.requestBodies
        #expect(bodies.count == 4, "one original action plus its exact-byte retry, alongside two proof requests")
        if bodies.count == 4 { #expect(bodies[1] == bodies[3]) }
    }
}

private actor DeadlineDirectTransport: HTTPTransport {
    private let mac: FakeDirectMac
    private(set) var actionTimeouts: [TimeInterval] = []

    init(mac: FakeDirectMac) { self.mac = mac }

    func send(_ request: URLRequest) async throws -> HTTPReply {
        if request.url?.path == "/direct/request", let body = request.httpBody,
           let envelope = try? JSONDecoder().decode(Envelope.self, from: body),
           envelope.requestID == "budget-action" {
            actionTimeouts.append(request.timeoutInterval)
        }
        return try await mac.send(request)
    }
}

private actor DeadlineRelayBridge: RelayOperations {
    private let mac: FakeDirectMac
    private let codec: EnvelopeCodec
    private let clock: MutationTestClock
    private let consumed: Duration
    private var sent: Envelope?

    init(mac: FakeDirectMac, codec: EnvelopeCodec, clock: MutationTestClock, consumed: Duration) {
        self.mac = mac; self.codec = codec; self.clock = clock; self.consumed = consumed
    }

    func perform(_ request: APIRequest) async throws -> APIResponse {
        let envelope = try codec.sealRequest(requestID: request.operationID, method: request.method,
            path: request.path, body: request.body)
        sent = envelope
        try await mac.deliverThroughTheRelay(envelope)
        clock.advance(by: consumed)
        return .unknown
    }
    func sentEnvelope(for requestID: String) async -> Envelope? { sent }
    func registerServicePush(token: String, environment: String, binding: String) async throws {}
    func deactivate() async {}
}

extension RoutedSessionTests {
    @Test(arguments: [false, true])
    func aMutationDoesNotRenewItsBudgetWhenTheRelayHandsOffToDirect(expired: Bool) async throws {
        let f = try await Fixture()
        let time = MutationTestClock()
        let recorded = DeadlineDirectTransport(mac: f.mac)
        let direct = DirectSession(macID: "mac-1", deviceID: "phone-1", signing: f.mac.device,
            agreement: f.mac.deviceAgreement, macSigning: f.mac.macSigning.publicKey,
            macAgreement: f.mac.macAgreement.publicKey, router: f.router,
            http: DirectHTTP(transport: recorded), pause: { _ in })
        let codec = EnvelopeCodec(macID: "mac-1", deviceID: "phone-1", signing: f.mac.device,
            agreement: f.mac.deviceAgreement, macSigning: f.mac.macSigning.publicKey,
            macAgreement: f.mac.macAgreement.publicKey, now: { UInt64(Date().timeIntervalSince1970 * 1000) })
        let relay = DeadlineRelayBridge(mac: f.mac, codec: codec, clock: time,
            consumed: .seconds(expired ? 121 : 115))
        let routed = RoutedSession(direct: direct, router: f.router, relay: relay, requestClock: time.clock)
        let request = try f.action("budget-action")
        let response = try await routed.perform(request)
        #expect(response == (expired ? .unknown : answered))
        let timeouts = await recorded.actionTimeouts
        #expect(timeouts == (expired ? [] : [5]), "the same 120-second budget must cover both carriers")
        #expect(await f.mac.executions(of: request.operationID) == 1)
        await routed.deactivate()
    }
}

private actor FractionalTimeoutTransport: HTTPTransport {
    private(set) var timeouts: [TimeInterval] = []
    func send(_ request: URLRequest) async throws -> HTTPReply {
        timeouts.append(request.timeoutInterval)
        return HTTPReply(status: 200, body: Data())
    }
}

extension DirectSessionTests {
    @Test func aPositiveFractionalTimeoutDoesNotBecomeAnUnlimitedHTTPTimeout() async throws {
        let transport = FractionalTimeoutTransport()
        _ = try await DirectHTTP(transport: transport).post(lan, path: "/direct/request", body: Data(), timeout: .milliseconds(250))
        #expect(await transport.timeouts == [0.25])
    }
}

private actor SharedDeadlineProbe: HTTPTransport {
    private let mac: FakeDirectMac
    private var held: CheckedContinuation<Void, Never>?
    let starts: AsyncStream<Void>
    private let continuation: AsyncStream<Void>.Continuation
    private(set) var inheritedBudget = false
    private(set) var probeCount = 0

    init(mac: FakeDirectMac) {
        self.mac = mac
        let pair = AsyncStream<Void>.makeStream(bufferingPolicy: .bufferingNewest(1))
        starts = pair.stream; continuation = pair.continuation
    }
    func send(_ request: URLRequest) async throws -> HTTPReply {
        if request.url?.path == "/direct/hello" {
            probeCount += 1
            inheritedBudget = MutationScope.current != nil
            await withCheckedContinuation { held = $0; continuation.yield(()) }
        }
        return try await mac.send(request)
    }
    func release() { held?.resume(); held = nil }
}

extension DirectSessionTests {
    @Test(arguments: [false, true])
    func oneProbeWaiterCanFinishWhileAnotherUsesTheSharedAnswer(cancelled: Bool) async throws {
        let mac = FakeDirectMac()
        await mac.answerHello(on: lan.host, with: await mac.identity)
        let probe = SharedDeadlineProbe(mac: mac)
        let http = DirectHTTP(transport: probe)
        let router = DirectRouter(pinned: await mac.identity, saved: [lan], http: http)
        let clock = MutationTestClock()
        let session = DirectSession(macID: "mac-1", deviceID: "phone-1", signing: mac.device,
            agreement: mac.deviceAgreement, macSigning: mac.macSigning.publicKey,
            macAgreement: mac.macAgreement.publicKey, router: router, http: http,
            pause: { _ in }, requestClock: clock.clock)
        let firstRunner = MutationRunner(clock: clock.clock)
        let nextRunner = MutationRunner(clock: clock.clock)
        var starts = await probe.starts.makeAsyncIterator()
        var nextStates = nextRunner.states.makeAsyncIterator()
        let firstRequest = try APIRequest(endpoint: Endpoint<Data>.data(.post, path: ["actions"], body: Data("{}".utf8)), operationID: "expired-probe")
        let nextRequest = try APIRequest(endpoint: Endpoint<Data>.data(.post, path: ["actions"], body: Data("{}".utf8)), operationID: "surviving-probe")
        let first = Task {
            try await firstRunner.run(unknown: { APIResponse.unknown }) { try await session.perform(firstRequest) }
        }
        #expect(await starts.next() != nil)
        clock.advance(by: .seconds(60))
        let next = Task {
            try await nextRunner.run(unknown: { APIResponse.unknown }) { try await session.perform(nextRequest) }
        }
        while let state = await nextStates.next() { if state.waiters == 1 { break } }
        if cancelled { first.cancel() } else { clock.advance(by: .seconds(60)) }
        let early = await first.result
        if case let .failure(error) = early {
            #expect(cancelled ? error is CancellationError : (error as? URLError)?.code == .timedOut)
        } else { Issue.record("the first waiter must finish before its probe permits dispatch") }
        // The nested session waiter may already have drained after its deadline. The
        // shared physical discovery remains held independently of either expired waiter.
        #expect(await probe.probeCount == 1)
        #expect(await mac.executions(of: "expired-probe") == 0)
        #expect(await !probe.inheritedBudget)
        await probe.release()
        let surviving = await next.result
        #expect((try? surviving.get()) == answered)
        await firstRunner.drain()
        await nextRunner.drain()
        #expect(await mac.executions(of: "expired-probe") == 0)
        #expect(await mac.executions(of: "surviving-probe") == 1)
        #expect(await probe.probeCount == 1)
        #expect(await firstRunner.runningWorkers.isEmpty)
        #expect(await nextRunner.runningWorkers.isEmpty)
        #expect(await firstRunner.timerTask == nil)
        #expect(await nextRunner.timerTask == nil)
        await session.deactivate()
    }
}

/// The Mac's direct port as a slow Mac or a refused local network leaves it.
private actor ImpairedDirectPort: HTTPTransport {
    enum Impairment { case none, slow, localNetworkDenied }
    private let mac: FakeDirectMac
    private var impairment = Impairment.none
    private(set) var requestTimeouts: [TimeInterval] = []

    init(mac: FakeDirectMac) { self.mac = mac }

    func impair(_ value: Impairment) { impairment = value }

    func send(_ request: URLRequest) async throws -> HTTPReply {
        switch impairment {
        case .none: return try await mac.send(request)
        case .localNetworkDenied:
            throw URLError(.notConnectedToInternet,
                           userInfo: ["_NSURLErrorNWPathKey": "unsatisfied (Local network prohibited), interface: en0[802.11]"])
        case .slow:
            guard request.url?.path == "/direct/request" else { return try await mac.send(request) }
            requestTimeouts.append(request.timeoutInterval)
            throw URLError(.timedOut)
        }
    }
}

private actor UnknownReadsRelay: RelayOperations {
    func perform(_ request: APIRequest) async throws -> APIResponse { .unknown }
    func sentEnvelope(for requestID: String) async -> Envelope? { nil }
    func registerServicePush(token: String, environment: String, binding: String) async throws {}
    func deactivate() async {}
}

extension RoutedSessionTests {
    private func impaired(_ f: Fixture) async -> (ImpairedDirectPort, DirectRouter, DirectSession) {
        let port = ImpairedDirectPort(mac: f.mac)
        let http = DirectHTTP(transport: port)
        let router = DirectRouter(pinned: await f.mac.identity, saved: [lan], http: http, network: { "home" })
        return (port, router, DirectSession(macID: "mac-1", deviceID: "phone-1", signing: f.mac.device,
            agreement: f.mac.deviceAgreement, macSigning: f.mac.macSigning.publicKey,
            macAgreement: f.mac.macAgreement.publicKey, router: router, http: http, pause: { _ in }))
    }

    /// On the same Wi-Fi as a slow Mac the iPhone waited four 20-second timeouts on the direct
    /// path before asking the relay, and the screen gave up first.
    @Test func aSlowMacHandsAReadToTheRelayAfterOneShortTry() async throws {
        let f = try await Fixture(saved: [lan])
        let (port, router, direct) = await impaired(f)
        let relay = FakeRelay()
        let routed = RoutedSession(direct: direct, router: router, relay: relay)
        for _ in 0..<200 where await routed.route != .localNetwork {
            _ = try? await routed.perform(f.read())
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(await routed.route == .localNetwork)
        await port.impair(.slow)
        #expect(try await routed.perform(f.read()) == viaRelay)
        #expect(await port.requestTimeouts == [TimeInterval(DirectSession.quickReadSlack)])
        #expect(await routed.route == .relay)
    }

    @Test func aReadTheRelayCouldNotGetIsAskedDirectly() async throws {
        let f = try await Fixture(saved: [lan])
        let routed = RoutedSession(direct: f.session, router: f.router, relay: UnknownReadsRelay(), probeInterval: .seconds(3600))
        #expect(try await routed.perform(f.read()) == answered)
        #expect(await routed.route == .localNetwork)
    }

    @Test func aRefusedLocalNetworkIsNamedWhenNothingElseAnswers() async throws {
        let f = try await Fixture(saved: [lan])
        let (port, router, direct) = await impaired(f)
        await port.impair(.localNetworkDenied)
        let routed = RoutedSession(direct: direct, router: router, relay: nil)
        await #expect(throws: DirectRouteError.localNetworkDenied) { try await routed.perform(f.read()) }
    }
}
