@testable import AlondraTransport
import AlondraTestSupport
import CryptoKit
import Foundation
import Synchronization
import Testing

private actor RecoveringDirectPeer: HTTPTransport {
    let macSigning = P256.Signing.PrivateKey()
    let macAgreement = P256.KeyAgreement.PrivateKey()
    let deviceSigning = P256.Signing.PrivateKey()
    let deviceAgreement = P256.KeyAgreement.PrivateKey()
    nonisolated let time = Mutex<UInt64>(1_000)
    private var original: Envelope?
    private var answer: Envelope?
    private var answerBytes: Data?
    let large: Bool
    private(set) var effects = 0
    private(set) var mutations = 0
    private(set) var lookups = 0
    let mismatch: String?

    init(mismatch: String? = nil, large: Bool = false) {
        self.mismatch = mismatch; self.large = large || mismatch == "offset" || mismatch == "total"
    }

    var identity: DirectMacIdentity {
        DirectMacIdentity(macID: "mac-fixture", name: "Fixture", signingKey: Base64URL.encode(macSigning.publicKey.x963Representation),
                          agreementKey: Base64URL.encode(macAgreement.publicKey.x963Representation))
    }

    func send(_ request: URLRequest) async throws -> HTTPReply {
        if request.url?.path == "/direct/hello" {
            return HTTPReply(status: 200, body: try JSONEncoder().encode(identity))
        }
        let envelope = try JSONDecoder().decode(Envelope.self, from: #require(request.httpBody))
        let now = time.withLock { $0 }
        let bytes = try envelope.open(senderKey: deviceSigning.publicKey, recipientKey: macAgreement,
            sender: "phone", recipient: "mac-fixture", route: "mac-fixture", nowMilliseconds: now)
        let payload = try #require(try JSONSerialization.jsonObject(with: bytes) as? [String: Any])
        let method = try #require(payload["method"] as? String)
        if method == "POST" {
            mutations += 1
            #expect(payload["path"] as? String == "/actions")
            if let original {
                let sameEnvelope = envelope == original
                #expect(sameEnvelope, "live attempts retain the exact original envelope")
            }
            else {
                effects += 1
                original = envelope
                answer = try Envelope.seal(route: "mac-fixture", sender: "mac-fixture", recipient: "phone",
                    requestID: envelope.requestID, kind: "response", expiresAtMilliseconds: 601_000,
                    plaintext: Data(#"{"status":200,"body": "#.utf8) + recoveryBody(large: large) + Data("}".utf8),
                    senderKey: macSigning, recipientKey: deviceAgreement.publicKey)
                answerBytes = try JSONEncoder().encode(#require(answer))
                #expect(try #require(answerBytes).count <= DirectHTTP.maxResponse, "the original answer fits the existing wire budget")
            }
            throw URLError(.networkConnectionLost)
        }
        #expect(method == "GET")
        lookups += 1
        let original = try #require(original)
        let answer = try #require(answer)
        let path = try #require(payload["path"] as? String)
        let components = try #require(URLComponents(string: path))
        #expect(components.path == "/operation-results/\(original.requestID)")
        #expect(components.queryItems?.first(where: { $0.name == "digest" })?.value == (try original.recoveryDigest()))
        #expect(components.queryItems?.first(where: { $0.name == "data_epoch" })?.value == "epoch")
        let inner = try JSONSerialization.jsonObject(with: JSONEncoder().encode(answer))
        var attestation: [String: Any] = [
            "request_id": mismatch == "id" ? "another" : original.requestID,
            "digest": mismatch == "digest" ? String(repeating: "0", count: 64) : try original.recoveryDigest(),
            "data_epoch": mismatch == "epoch" ? "replacement" : "epoch", "response": inner]
        if large {
            let bytes = try #require(answerBytes)
            let offset = components.queryItems?.first(where: { $0.name == "offset" })?.value.flatMap(Int.init) ?? 0
            #expect(offset % (256 * 1024) == 0)
            attestation.removeValue(forKey: "response")
            attestation["offset"] = mismatch == "offset" ? offset + 1 : offset
            attestation["total"] = mismatch == "total" ? DirectHTTP.maxResponse + 1 : bytes.count
            attestation["chunk"] = bytes.subdata(in: offset..<min(bytes.count, offset + 256 * 1024)).base64EncodedString()
        }
        let outer = try Envelope.seal(route: "mac-fixture", sender: "mac-fixture", recipient: "phone", requestID: envelope.requestID,
            kind: "response", expiresAtMilliseconds: now + 120_000,
            plaintext: JSONSerialization.data(withJSONObject: ["status": 200, "body": attestation]),
            senderKey: macSigning, recipientKey: deviceAgreement.publicKey)
        let encoded = try JSONEncoder().encode(outer)
        #expect(encoded.count <= DirectHTTP.maxResponse, "each fresh recovery response fits the unchanged limit")
        return HTTPReply(status: 200, body: encoded)
    }

    func session(directory: URL) throws -> DirectSession {
        let http = DirectHTTP(transport: self)
        let router = DirectRouter(pinned: identity,
            saved: [DirectEndpoint(host: "127.0.0.1", port: 47810, kind: .localNetwork)], http: http)
        let scope = RelayOutboxScope(relay: URL(string: "https://relay.fixture.invalid")!, macID: "mac-fixture",
            deviceID: "phone", credentialRevision: "approved-key")
        return DirectSession(macID: "mac-fixture", deviceID: "phone", signing: deviceSigning, agreement: deviceAgreement,
            macSigning: macSigning.publicKey, macAgreement: macAgreement.publicKey, router: router, http: http,
            journal: try EnvelopeJournal(scope: scope, directory: directory),
            now: { self.time.withLock { $0 } }, pause: { _ in })
    }
}

private func recoveryBody(large: Bool) -> Data {
    if !large { return Data(#"{"applied":true}"#.utf8) }
    return Data((#"{"applied":true,"text":""# + String(repeating: "x", count: 760 * 1024 - 128) + #""}"#).utf8)
}

@Suite(.temporaryFiles) struct DurableActionRecoveryTests {
    private func request() throws -> APIRequest {
        try APIRequest(endpoint: Endpoint<Data>.data(.post, path: ["actions"],
            body: Data(#"{"command_id":"original","data_epoch":"epoch"}"#.utf8)), operationID: "original")
    }

    @Test(arguments: [false, true])
    func relaunchRecoversExpiredActionWithFreshGETAndOneEffect(large: Bool) async throws {
        let directory = try TestFiles.directory()
        let peer = RecoveringDirectPeer(large: large)
        let first = try await peer.session(directory: directory)
        #expect(try await first.perform(request()) == .unknown)
        await first.deactivate()
        peer.time.withLock { $0 = 700_000 }
        let relaunched = try await peer.session(directory: directory)
        #expect(try await relaunched.recover(request()) == .completed(status: 200, body: recoveryBody(large: large)))
        #expect(try await relaunched.perform(request()) == .completed(status: 200, body: recoveryBody(large: large)))
        #expect(await peer.effects == 1)
        #expect(await peer.mutations == 4, "only the original live-session exact-envelope attempts; none after relaunch")
        #expect(await peer.lookups == (large ? 8 : 2))
        await relaunched.deactivate()
    }

    @Test(arguments: ["id", "digest", "epoch", "offset", "total"])
    func aFreshAnswerCannotAttestToAnotherOperationOrEpoch(mismatch: String) async throws {
        let directory = try TestFiles.directory()
        let peer = RecoveringDirectPeer(mismatch: mismatch)
        let first = try await peer.session(directory: directory)
        #expect(try await first.perform(request()) == .unknown)
        await first.deactivate()
        peer.time.withLock { $0 = 700_000 }
        let relaunched = try await peer.session(directory: directory)
        await #expect(throws: RelayWireError.invalidResponse) { try await relaunched.recover(request()) }
        #expect(await peer.effects == 1)
        #expect(await peer.mutations == 4)
        await relaunched.deactivate()
    }

    @Test func digestUsesTheSameFramingAsTheDaemon() throws {
        let envelope = Envelope(route: "mac-fixture", sender: "phone-fixture", recipient: "mac-fixture",
            requestID: "fixture-operation", kind: "request", expiresAtMilliseconds: 12345,
            ephemeralKey: "BGsX0fLhLEJH-Lzm5WOkQPJ3A32BLeszoPShOUXYmMKWT-NC4v4af5uO5-tKfA-eFivOM1drMV7Oy7ZAaDe_UfU",
            nonce: "AAECAwQFBgcICQoL", ciphertext: "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8",
            signature: "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8gISIjJCUmJygpKissLS4vMDEyMzQ1Njc4OTo7PD0-Pw")
        #expect(try envelope.recoveryDigest() == "5dca4fef6c944cf38f66e0171cab83be4e9f4d3a5d71b523bb2f6a3104e4e19d")
    }
}

private actor RecoveringLoopbackPeer: HTTPTransport {
    let key: P256.Signing.PublicKey
    let mismatchedEpoch: Bool
    private(set) var effects = 0
    private(set) var mutationCount = 0
    private(set) var lookupCount = 0
    private var body: Data?
    init(key: P256.Signing.PublicKey, mismatchedEpoch: Bool) {
        self.key = key; self.mismatchedEpoch = mismatchedEpoch
    }
    func send(_ request: URLRequest) async throws -> HTTPReply {
        let url = try #require(request.url)
        let method = try #require(request.httpMethod)
        let target = url.path + (url.query.map { "?" + $0 } ?? "")
        let timestamp = try #require(request.value(forHTTPHeaderField: "X-Alondra-Timestamp"))
        let signature = try #require(request.value(forHTTPHeaderField: "X-Alondra-Signature").flatMap { Data(base64Encoded: $0) })
        #expect(request.value(forHTTPHeaderField: "X-Alondra-Device") == "fixture-mac-client")
        #expect(key.isValidSignature(try P256.Signing.ECDSASignature(derRepresentation: signature),
            for: LoopbackSignature.message(method: method, target: target, timestamp: timestamp, body: request.httpBody ?? Data())))
        if method == "POST" {
            #expect(url.path == "/actions")
            mutationCount += 1
            effects += 1
            body = try #require(request.httpBody)
            throw URLError(.networkConnectionLost)
        }
        #expect(method == "GET")
        #expect(url.path == "/operation-results/5f0c2d9e-8d0b-4c71-9b0e-2b6f5d8f1a10")
        lookupCount += 1
        let original = try #require(body)
        let digest = SHA256.hash(data: original).map { String(format: "%02x", $0) }.joined()
        let components = try #require(URLComponents(url: url, resolvingAgainstBaseURL: false))
        #expect(components.queryItems?.first(where: { $0.name == "digest" })?.value == digest)
        #expect(components.queryItems?.first(where: { $0.name == "data_epoch" })?.value == "epoch")
        return HTTPReply(status: 200, body: try JSONSerialization.data(withJSONObject: [
            "request_id": "5f0c2d9e-8d0b-4c71-9b0e-2b6f5d8f1a10", "digest": digest,
            "data_epoch": mismatchedEpoch ? "replacement" : "epoch", "response": ["outcome": "applied"]]))
    }
}

extension DurableActionRecoveryTests {
    @Test(arguments: [false, true])
    func localMacRelaunchRecoversOnlyWithSignedGETAndMatchingEpoch(mismatchedEpoch: Bool) async throws {
        let directory = try TestFiles.directory()
        let key = P256.Signing.PrivateKey()
        let identity = LoopbackIdentityStore(directory: directory, makeKey: { .software(key) })
        _ = try await identity.signingKey()
        try await identity.save(deviceID: "fixture-mac-client")
        let peer = RecoveringLoopbackPeer(key: key.publicKey, mismatchedEpoch: mismatchedEpoch)
        let original = try APIRequest(endpoint: Endpoint<Data>.data(.post, path: ["actions"],
            body: Data(#"{"command_id":"5f0c2d9e-8d0b-4c71-9b0e-2b6f5d8f1a10","data_epoch":"epoch"}"#.utf8)),
            operationID: "5f0c2d9e-8d0b-4c71-9b0e-2b6f5d8f1a10")
        let first = LoopbackSession(identity: identity, deviceName: "Fixture", requestClock: .continuous, transport: peer)
        #expect(try await first.perform(original) == .unknown)
        await first.deactivate()
        let restoredIdentity = LoopbackIdentityStore(directory: directory, makeKey: { .software(key) })
        let relaunched = LoopbackSession(identity: restoredIdentity, deviceName: "Fixture", requestClock: .continuous, transport: peer)
        if mismatchedEpoch {
            await #expect(throws: LoopbackSessionError.invalidResponse) { try await relaunched.recover(original) }
        } else {
            #expect(try await relaunched.recover(original) == .completed(status: 200, body: Data(#"{"outcome":"applied"}"#.utf8)))
        }
        #expect(await peer.effects == 1)
        #expect(await peer.mutationCount == 1)
        #expect(await peer.lookupCount == 1)
        await relaunched.deactivate()
    }
}
