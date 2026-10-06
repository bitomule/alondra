@testable import AlondraTransport
import AlondraContract
import CryptoKit
import Foundation
import Testing

/// A Mac's direct port in memory: answers `hello` per address and runs each request ID once.
private actor FakeDirectMac: HTTPTransport {
    let macSigning = P256.Signing.PrivateKey()
    let macAgreement = P256.KeyAgreement.PrivateKey()
    let device = P256.Signing.PrivateKey()
    let deviceAgreement = P256.KeyAgreement.PrivateKey()
    var helloByHost: [String: DirectMacIdentity] = [:]
    var failNextRequests = 0
    var requestStatus = 200
    private(set) var requestBodies: [Data] = []
    private(set) var hosts: [String] = []
    private var journal: [String: Data] = [:]
    private var executions = 0

    var identity: DirectMacIdentity {
        DirectMacIdentity(macID: "mac-1", name: "Studio",
                          signingKey: Base64URL.encode(macSigning.publicKey.x963Representation),
                          agreementKey: Base64URL.encode(macAgreement.publicKey.x963Representation))
    }

    func answerHello(on host: String, with identity: DirectMacIdentity?) { helloByHost[host] = identity }
    func failRequests(_ count: Int) { failNextRequests = count }
    func answerRequests(with status: Int) { requestStatus = status }
    var executed: Int { executions }

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
        guard requestStatus == 200 else { return HTTPReply(status: requestStatus, body: Data("{}".utf8)) }
        let envelope = try JSONDecoder().decode(Envelope.self, from: request.httpBody!)
        if let stored = journal[envelope.requestID] { return HTTPReply(status: 200, body: stored) }
        _ = try envelope.open(senderKey: device.publicKey, recipientKey: macAgreement, sender: "phone-1",
                              recipient: "mac-1", route: "mac-1", nowMilliseconds: nil)
        executions += 1
        let plain = try JSONSerialization.data(withJSONObject: ["status": 200, "body": ["ok": true]])
        let response = try Envelope.seal(route: "mac-1", sender: "mac-1", recipient: "phone-1",
            requestID: envelope.requestID, kind: "response",
            expiresAtMilliseconds: UInt64(Date().timeIntervalSince1970 * 1000) + 60_000,
            plaintext: plain, senderKey: macSigning, recipientKey: deviceAgreement.publicKey)
        let body = try JSONEncoder().encode(response)
        journal[envelope.requestID] = body
        return HTTPReply(status: 200, body: body)
    }
}

private struct Fixture {
    let mac = FakeDirectMac()
    let router: DirectRouter
    let session: DirectSession

    init(candidates: [DirectEndpoint] = [DirectEndpoint(host: "192.168.1.5", port: 47810, kind: .localNetwork)]) async throws {
        let http = DirectHTTP(transport: mac)
        router = DirectRouter(pinned: await mac.identity, candidates: candidates, http: http)
        session = DirectSession(macID: "mac-1", deviceID: "phone-1", signing: mac.device,
            agreement: mac.deviceAgreement, macSigning: mac.macSigning.publicKey,
            macAgreement: mac.macAgreement.publicKey, router: router, http: http, pause: { _ in })
        for candidate in candidates where candidate.host != "100.100.1.1" {
            await mac.answerHello(on: candidate.host, with: await mac.identity)
        }
    }

    func action(_ id: String, body: String = #"{"command":"a"}"#) throws -> APIRequest {
        try APIRequest(endpoint: Endpoint<Data>.data(.post, path: ["actions"], body: Data(body.utf8)), operationID: id)
    }
}

@Suite struct DirectSessionTests {
    @Test func aLostAnswerIsResentAsTheSameBytesAndRunsOnce() async throws {
        let f = try await Fixture()
        await f.mac.failRequests(1)
        let response = try await f.session.perform(f.action("op-1"))
        #expect(response == .completed(status: 200, body: Data(#"{"ok":true}"#.utf8)))
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
        await f.mac.failRequests(3)
        #expect(try await f.session.perform(f.action("op-1")) == .unknown)
    }

    @Test func theMacRefusingTheDeviceIsUnauthorized() async throws {
        let f = try await Fixture()
        await f.mac.answerRequests(with: 401)
        await #expect(throws: DirectSessionError.unauthorized) { try await f.session.perform(f.action("op-1")) }
    }

    @Test func theMacsIdempotencyConflictIsAnIdentityConflict() async throws {
        let f = try await Fixture()
        await f.mac.answerRequests(with: 409)
        await #expect(throws: RelaySessionError.operationIdentityConflict) { try await f.session.perform(f.action("op-1")) }
    }
}

@Suite struct DirectRouterTests {
    private let lan = DirectEndpoint(host: "192.168.1.5", port: 47810, kind: .localNetwork)
    private let tailnetIP = DirectEndpoint(host: "100.100.1.1", port: 47810, kind: .tailscale)
    private let tailnetName = DirectEndpoint(host: "studio.tail1.ts.net", port: 47810, kind: .tailscale)

    @Test func prefersTheLANThenTheTailnetIPThenItsName() async throws {
        let f = try await Fixture(candidates: [tailnetName, tailnetIP, lan])
        #expect(await f.router.known == [lan, tailnetIP, tailnetName])
        #expect(try await f.router.endpoint() == lan)
        #expect(await f.router.route == .localNetwork)
    }

    @Test func failsOverToTheTailnetWhenTheLANStopsAnswering() async throws {
        let f = try await Fixture(candidates: [lan, tailnetName])
        #expect(try await f.router.endpoint() == lan)
        await f.mac.answerHello(on: lan.host, with: nil)
        await f.router.invalidate()
        #expect(try await f.router.endpoint() == tailnetName)
        #expect(await f.router.route == .tailscale)
    }

    @Test func anotherMacAtAKnownAddressIsSkipped() async throws {
        let f = try await Fixture(candidates: [lan])
        let stranger = DirectMacIdentity(macID: "mac-2", name: "Other",
            signingKey: Base64URL.encode(P256.Signing.PrivateKey().publicKey.x963Representation),
            agreementKey: Base64URL.encode(P256.KeyAgreement.PrivateKey().publicKey.x963Representation))
        await f.mac.answerHello(on: lan.host, with: stranger)
        await #expect(throws: DirectRouteError.unreachable) { try await f.router.endpoint() }
    }

    @Test func theSameMacIDWithOtherKeysStopsEveryRoute() async throws {
        let f = try await Fixture(candidates: [lan, tailnetName])
        let pinned = await f.mac.identity
        let impostor = DirectMacIdentity(macID: pinned.macID, name: pinned.name,
            signingKey: Base64URL.encode(P256.Signing.PrivateKey().publicKey.x963Representation),
            agreementKey: pinned.agreementKey)
        await f.mac.answerHello(on: lan.host, with: impostor)
        await #expect(throws: DirectRouteError.identityChanged) { try await f.router.endpoint() }
        await #expect(throws: DirectRouteError.identityChanged) { try await f.router.endpoint() }
        await #expect(throws: DirectRouteError.identityChanged) { try await f.session.perform(f.action("op-1")) }
        #expect(await f.mac.requestBodies.isEmpty)
    }

    @Test func noAnsweringAddressMeansNotOnThisNetwork() async throws {
        let f = try await Fixture(candidates: [tailnetIP])
        await #expect(throws: DirectRouteError.unreachable) { try await f.session.perform(f.action("op-1")) }
        #expect(await f.mac.requestBodies.isEmpty)
    }

    @Test func routesFromTheMacBecomeEndpoints() throws {
        let reply = try JSONDecoder().decode(DirectRoutesReply.self, from: Data(
            #"{"port":47810,"lan":["192.168.1.5"],"tailscale":{"ip":"100.100.1.1","name":"studio.tail1.ts.net."}}"#.utf8))
        #expect(reply.endpoints == [lan, tailnetIP, tailnetName])
    }
}
