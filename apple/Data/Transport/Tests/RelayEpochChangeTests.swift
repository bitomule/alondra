@testable import AlondraTransport
import CryptoKit
import Foundation
import Testing

/// A relay that, like the real one after a restart, refuses a request signed for its previous
/// epoch with a 401 that names the change and the epoch it uses now. It can hold one signed
/// request in flight, and records the epoch each signed request carried, by path.
private actor RestartingRelay: HTTPTransport {
    private var epoch = Data(repeating: 7, count: 16)
    private(set) var bootstraps = 0
    private(set) var signedEpochs: [Data] = []
    private(set) var signedEpochsByPath: [String: [Data]] = [:]
    private var holdPath: String?
    private var held: CheckedContinuation<Void, Never>?
    private var holding: CheckedContinuation<Void, Never>?

    func restart() { epoch = Data(repeating: epoch[0] + 1, count: 16) }
    func hold(_ path: String) { holdPath = path }
    func untilHeld() async { if held == nil { await withCheckedContinuation { holding = $0 } } }
    func release() { held?.resume(); held = nil }

    func send(_ request: URLRequest) async throws -> HTTPReply {
        let url = try #require(request.url)
        if url.path == "/v1/bootstrap" {
            bootstraps += 1
            return HTTPReply(status: 200, body: Data("{\"protocol\":1,\"epoch\":\"\(Base64URL.encode(epoch))\"}".utf8))
        }
        let signed = try Base64URL.decode(#require(request.value(forHTTPHeaderField: "x-relay-epoch")))
        if url.path == holdPath {
            holdPath = nil
            await withCheckedContinuation { held = $0; holding?.resume(); holding = nil }
        }
        signedEpochs.append(signed)
        signedEpochsByPath[url.path, default: []].append(signed)
        guard signed == epoch else {
            return HTTPReply(status: 401, body: Data("{\"error\":\"relay_epoch_changed\",\"epoch\":\"\(Base64URL.encode(epoch))\"}".utf8))
        }
        if url.path == "/v1/push/register" { return HTTPReply(status: 204, body: Data()) }
        return HTTPReply(status: 200, body: Data("{\"request_id\":\"\(url.lastPathComponent)\",\"status\":\"completed\",\"revision\":1}".utf8))
    }
}

private actor Refusals {
    private(set) var count = 0
    func add() { count += 1 }
}

private actor AccessToken {
    private(set) var value = "token-1"
    func renew() { value = "token-2" }
}

@Suite struct RelayEpochChangeTests {
    private let old = Data(repeating: 7, count: 16), new = Data(repeating: 8, count: 16)

    private func setup() throws -> (any Transport, RestartingRelay, Refusals, AccessToken) {
        let signing = try P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 3, count: 32))
        let relay = RestartingRelay()
        let refusals = Refusals()
        let token = AccessToken()
        let transport = try AlondraAPIClient.makeTransport(base: URL(string: "https://relay.fixture.invalid")!, deviceID: "phone-1",
            signer: { try signing.signature(for: $0).rawRepresentation }, bearer: { await token.value },
            bearerRefused: { await refusals.add() }, transport: relay, now: { 1_000 })
        return (transport, relay, refusals, token)
    }

    @Test func aRequestSignedBeforeARelayRestartIsSignedAgainInsteadOfFailing() async throws {
        let (transport, relay, refusals, _) = try setup()
        _ = try await transport.status(requestID: "before")
        await relay.restart()
        let job = try await transport.status(requestID: "after")
        #expect(job.requestID == "after")
        #expect(await relay.signedEpochs == [old, old, new])
        #expect(await relay.bootstraps == 2)
        #expect(await refusals.count == 0, "a stale epoch is not a refused access token")
    }

    @Test func aRestartBeforeTheFirstSignedRequestDoesNotRenewTheAccessToken() async throws {
        let (transport, relay, refusals, _) = try setup()
        _ = try await transport.bootstrap()
        await relay.restart()
        let job = try await transport.status(requestID: "first")
        #expect(job.requestID == "first")
        #expect(await relay.bootstraps == 2)
        #expect(await refusals.count == 0, "a stale epoch is not a refused access token")
    }

    @Test func aRequestStillInFlightWhenAnotherRenewedTheTokenIsSignedAgainForTheNewEpoch() async throws {
        let (transport, relay, refusals, token) = try setup()
        _ = try await transport.status(requestID: "before")
        await relay.hold("/v1/requests/slow")
        let slow = Task { try await transport.status(requestID: "slow") }
        await relay.untilHeld()
        await token.renew()
        _ = try await transport.status(requestID: "renewed")
        await relay.restart()
        await relay.release()
        #expect(try await slow.value.requestID == "slow")
        #expect(await relay.signedEpochsByPath["/v1/requests/slow"] == [old, new])
        #expect(await relay.bootstraps == 2)
        #expect(await refusals.count == 0, "a stale epoch is not a refused access token")
    }

    @Test func aCancelledRequestRefusedForTheEpochLeavesTheNextOneSignedForTheNewEpoch() async throws {
        let (transport, relay, refusals, _) = try setup()
        _ = try await transport.status(requestID: "before")
        await relay.hold("/v1/requests/cancelled")
        let cancelled = Task { try await transport.status(requestID: "cancelled") }
        await relay.untilHeld()
        await relay.restart()
        cancelled.cancel()
        await relay.release()
        _ = await cancelled.result
        let job = try await transport.status(requestID: "next")
        #expect(job.requestID == "next")
        #expect(await relay.signedEpochsByPath["/v1/requests/next"] == [new])
        #expect(await refusals.count == 0, "a stale epoch is not a refused access token")
    }

    @Test func alertsRegisteredAcrossARestartWithARenewedTokenReachTheRelay() async throws {
        let (transport, relay, refusals, token) = try setup()
        let registration = try RelayPushRegistration(token: String(repeating: "a", count: 64), environment: "production",
                                                     pushBinding: Base64URL.encode(Data(repeating: 1, count: 32)))
        _ = try await transport.status(requestID: "before")
        await relay.hold("/v1/push/register")
        let register = Task { try await transport.registerPush(registration) }
        await relay.untilHeld()
        await token.renew()
        _ = try await transport.status(requestID: "renewed")
        await relay.restart()
        await relay.release()
        try await register.value
        #expect(await relay.signedEpochsByPath["/v1/push/register"] == [old, new])
        #expect(await refusals.count == 0, "a stale epoch is not a refused access token")
    }
}
