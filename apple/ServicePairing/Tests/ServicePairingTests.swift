import CryptoKit
import Foundation
import Testing
import AlondraAPIClient
@testable import AlondraServicePairing

private let macSigning = try! P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 1, count: 32))
private let macAgreement = try! P256.KeyAgreement.PrivateKey(rawRepresentation: Data(repeating: 2, count: 32))
private let fixedNow: UInt64 = 1_800_000_000_000

private func invitation(expiry: UInt64 = fixedNow + 600_000) throws -> String {
    let object: [String: Any] = [
        "version": 1, "url": "https://relay.example", "mac_id": "mac-1",
        "mac_signing_key": PairingWire.encode(macSigning.publicKey.x963Representation),
        "mac_agreement_key": PairingWire.encode(macAgreement.publicKey.x963Representation),
        "session_id": "pair-test", "secret": PairingWire.encode(Data(repeating: 9, count: 16)),
        "expires_at_ms": expiry,
    ]
    return String(decoding: try JSONSerialization.data(withJSONObject: object), as: UTF8.self)
}

private final class MemoryStore: ApprovedPairingStore, @unchecked Sendable {
    private let lock = NSLock()
    private var value: ApprovedServicePairing?
    func load() throws -> ApprovedServicePairing? { lock.withLock { value } }
    func save(_ pairing: ApprovedServicePairing) throws { lock.withLock { value = pairing } }
    func delete() throws { lock.withLock { value = nil } }
}

private actor FakeRelay: ServiceHTTPTransport {
    enum ApprovalMode { case valid, wrongMacKey, wrongOwner, unsupportedProtocol }
    let mode: ApprovalMode
    var candidate: PairingCandidate?
    var requests: [URLRequest] = []

    init(mode: ApprovalMode = .valid) { self.mode = mode }

    func send(_ request: URLRequest) async throws -> ServiceHTTPResponse {
        requests.append(request)
        let path = request.url!.path
        if path == "/v1/bootstrap", mode == .unsupportedProtocol {
            return answer(200, Data(#"{"protocol":2,"epoch":"BwcHBwcHBwcHBwcHBwcHBw"}"#.utf8), request)
        }
        if path == "/v1/bootstrap" { return answer(200, Data(#"{"protocol":1,"epoch":"BwcHBwcHBwcHBwcHBwcHBw"}"#.utf8), request) }
        if path.hasSuffix("/candidates") {
            candidate = try JSONDecoder().decode(PairingCandidate.self, from: request.httpBody!)
            try verifySignedHTTP(request, signingKey: candidate!.signingKey)
            return answer(202, Data(), request)
        }
        guard path.hasSuffix("/approval"), let candidate else { return answer(404, Data(), request) }
        try verifySignedHTTP(request, signingKey: candidate.signingKey)
        let payload: [String: Any] = [
            "v": 1, "session_id": "pair-test",
            "owner_sub": mode == .wrongOwner ? "other-owner" : "owner-1",
            "mac_id": "mac-1",
            "mac_signing_key": mode == .wrongMacKey ? PairingWire.encode(P256.Signing.PrivateKey().publicKey.x963Representation) : PairingWire.encode(macSigning.publicKey.x963Representation),
            "mac_agreement_key": PairingWire.encode(macAgreement.publicKey.x963Representation),
            "device_id": candidate.deviceID,
            "phone_signing_key": candidate.signingKey,
            "phone_agreement_key": try decryptedCandidateAgreement(candidate.envelope),
            "expires_at_ms": fixedNow + 600_000,
        ]
        let approvalData = try JSONSerialization.data(withJSONObject: payload)
        let approval = try JSONDecoder().decode(PairingApproval.self, from: approvalData)
        let proof: [String: Any] = [
            "approval": payload,
            "signature": PairingWire.encode(try macSigning.signature(for: approval.signedBytes()).rawRepresentation),
        ]
        return answer(200, try JSONSerialization.data(withJSONObject: proof), request)
    }

    private func decryptedCandidateAgreement(_ envelope: EnvelopeV1) throws -> String {
        let epk = try P256.KeyAgreement.PublicKey(x963Representation: PairingWire.decode(envelope.ephemeralKey)!)
        let nonce = PairingWire.decode(envelope.nonce)!
        var header = Data("alondra-e2e-envelope-v1".utf8)
        for value in [Data([1]), Data(envelope.route.utf8), Data(envelope.sender.utf8),
                      Data(envelope.recipient.utf8), Data(envelope.requestID.utf8),
                      Data(envelope.kind.utf8), PairingWire.bigEndian(envelope.expiresAtMilliseconds),
                      PairingWire.decode(envelope.ephemeralKey)!, nonce] {
            PairingWire.appendFrame(value, to: &header)
        }
        let key = try macAgreement.sharedSecretFromKeyAgreement(with: epk)
            .hkdfDerivedSymmetricKey(using: SHA256.self,
                salt: Data(SHA256.hash(data: header)),
                sharedInfo: Data("alondra-e2e-key-v1".utf8), outputByteCount: 32)
        let signing = try P256.Signing.PublicKey(x963Representation: PairingWire.decode(candidate!.signingKey)!)
        var signed = header
        PairingWire.appendFrame(PairingWire.decode(envelope.ciphertext)!, to: &signed)
        let sig = try P256.Signing.ECDSASignature(rawRepresentation: PairingWire.decode(envelope.signature)!)
        #expect(signing.isValidSignature(sig, for: signed))
        let sealed = try ChaChaPoly.SealedBox(combined: nonce + PairingWire.decode(envelope.ciphertext)!)
        let plaintext = try ChaChaPoly.open(sealed, using: key, authenticating: header)
        let candidate = try JSONSerialization.jsonObject(with: plaintext) as! [String: String]
        #expect(candidate["owner_sub"] == "owner-1")
        #expect(candidate["session_id"] == "pair-test")
        return candidate["agreement_key"]!
    }

    private func verifySignedHTTP(_ request: URLRequest, signingKey: String) throws {
        let deviceID = try #require(request.value(forHTTPHeaderField: "X-Device-ID"))
        let timestamp = try #require(request.value(forHTTPHeaderField: "X-Device-Timestamp-Ms"))
        let epochHeader = try #require(request.value(forHTTPHeaderField: "X-Relay-Epoch"))
        let nonceHeader = try #require(request.value(forHTTPHeaderField: "X-Device-Nonce"))
        let signatureHeader = try #require(request.value(forHTTPHeaderField: "X-Device-Signature"))
        let epoch = try #require(PairingWire.decode(epochHeader))
        let nonce = try #require(PairingWire.decode(nonceHeader))
        let signatureBytes = try #require(PairingWire.decode(signatureHeader))
        #expect(epoch == Data(repeating: 7, count: 16))
        #expect(nonce.count == 16)
        var challenge = Data("alondra-http-auth-v1".utf8)
        for value in [Data(deviceID.utf8), Data((request.httpMethod ?? "").utf8),
                      Data(request.url!.path.utf8), epoch,
                      Data(SHA256.hash(data: request.httpBody ?? Data())),
                      Data(timestamp.utf8), nonce] {
            PairingWire.appendFrame(value, to: &challenge)
        }
        let key = try P256.Signing.PublicKey(x963Representation: PairingWire.decode(signingKey)!)
        let signature = try P256.Signing.ECDSASignature(rawRepresentation: signatureBytes)
        #expect(key.isValidSignature(signature, for: challenge))
    }

    private func answer(_ status: Int, _ body: Data, _ request: URLRequest) -> ServiceHTTPResponse {
        ServiceHTTPResponse(status: status, body: body)
    }
}

@Test func submitsEncryptedCandidateAndAcceptsPinnedMacApproval() async throws {
    let relay = FakeRelay()
    let store = MemoryStore()
    let client = ServicePairingClient(transport: relay, store: store,
        now: { Date(timeIntervalSince1970: Double(fixedNow) / 1_000) })
    let auth = try PairingAuthorization(bearerToken: "oidc-token", ownerSub: "owner-1")
    let pending = try await client.begin(qrPayload: invitation(), authorization: auth,
        displayName: "David’s iPhone")
    #expect(try store.load() == nil)
    let candidate = try #require(await relay.candidate)
    let publicKey = PairingWire.decode(candidate.signingKey)!
    #expect(pending.comparisonCode == PairingWire.encode(Data(SHA256.hash(data: publicKey).prefix(6))))
    #expect(candidate.envelope.kind == "pairing")
    #expect(candidate.envelope.expiresAtMilliseconds <= pending.expiresAtMs)
    #expect(!String(decoding: requestsBody(await relay.requests), as: UTF8.self).contains("David’s iPhone"))
    let approved = try await client.waitForApproval(pending.attemptID)
    #expect(approved.macID == "mac-1")
    #expect(approved.ownerSub == "owner-1")
    #expect(try store.load()?.deviceID == approved.deviceID)
    let requests = await relay.requests
    #expect(requests.filter { $0.url?.path.contains("pairings") == true }.allSatisfy {
        $0.value(forHTTPHeaderField: "Authorization") == "Bearer oidc-token" &&
        $0.value(forHTTPHeaderField: "X-Device-Signature") != nil
    })
}

private func requestsBody(_ requests: [URLRequest]) -> Data {
    requests.first(where: { $0.url?.path.hasSuffix("/candidates") == true })?.httpBody ?? Data()
}

@Test(arguments: [FakeRelay.ApprovalMode.wrongMacKey, .wrongOwner])
private func rejectsForgedOrOtherAccountApproval(mode: FakeRelay.ApprovalMode) async throws {
    let store = MemoryStore()
    let client = ServicePairingClient(transport: FakeRelay(mode: mode), store: store,
        now: { Date(timeIntervalSince1970: Double(fixedNow) / 1_000) })
    let auth = try PairingAuthorization(bearerToken: "oidc-token", ownerSub: "owner-1")
    let pending = try await client.begin(qrPayload: invitation(), authorization: auth,
        displayName: "iPhone")
    await #expect(throws: ServicePairingError.invalidApproval) {
        try await client.waitForApproval(pending.attemptID)
    }
    #expect(try store.load() == nil)
}

@Test func rejectsExpiredInvitationBeforeSendingCandidate() async throws {
    let relay = FakeRelay()
    let client = ServicePairingClient(transport: relay, store: MemoryStore(),
        now: { Date(timeIntervalSince1970: Double(fixedNow) / 1_000) })
    let auth = try PairingAuthorization(bearerToken: "oidc-token", ownerSub: "owner-1")
    await #expect(throws: ServicePairingError.invalidInvitation) {
        try await client.begin(qrPayload: invitation(expiry: fixedNow + 1_000),
            authorization: auth, displayName: "iPhone")
    }
    #expect(await relay.requests.isEmpty)
}

@Test func rejectsUnsupportedProtocolBeforeSendingCandidate() async throws {
    let relay = FakeRelay(mode: .unsupportedProtocol)
    let store = MemoryStore()
    let client = ServicePairingClient(transport: relay, store: store,
        now: { Date(timeIntervalSince1970: Double(fixedNow) / 1_000) })
    let auth = try PairingAuthorization(bearerToken: "oidc-token", ownerSub: "owner-1")
    await #expect(throws: RelayWireError.invalidResponse) {
        try await client.begin(qrPayload: invitation(), authorization: auth, displayName: "iPhone")
    }
    #expect(await relay.requests.map { $0.url?.path } == ["/v1/bootstrap"])
    #expect(await relay.candidate == nil)
    #expect(try store.load() == nil)
}
