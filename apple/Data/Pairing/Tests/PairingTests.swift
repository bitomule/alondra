import CryptoKit
import Foundation
import Synchronization
import Testing
import AlondraTransport
@testable import AlondraPairing

private let macSigning = try! P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 1, count: 32))
private let macAgreement = try! P256.KeyAgreement.PrivateKey(rawRepresentation: Data(repeating: 2, count: 32))
private let fixedNow: UInt64 = 1_800_000_000_000

private func invitation(expiry: UInt64 = fixedNow + 600_000, irohEndpoint: String? = nil) throws -> String {
    var object: [String: Any] = [
        "version": 1, "url": "https://relay.example", "mac_id": "mac-1",
        "mac_signing_key": PairingWire.encode(macSigning.publicKey.x963Representation),
        "mac_agreement_key": PairingWire.encode(macAgreement.publicKey.x963Representation),
        "session_id": "pair-test", "secret": PairingWire.encode(Data(repeating: 9, count: 16)),
        "expires_at_ms": expiry,
    ]
    if let irohEndpoint { object["mac_iroh_endpoint"] = irohEndpoint }
    return String(decoding: try JSONSerialization.data(withJSONObject: object), as: UTF8.self)
}

private final class MemoryStore: ApprovedPairingStore {
    private let value = Mutex(ApprovedPairingCollection())
    private let writes = Mutex(0)
    var writeCount: Int { writes.withLock { $0 } }
    func loadCollection() throws -> ApprovedPairingCollection { value.withLock { $0 } }
    func load() throws -> ApprovedServicePairing? { value.withLock { $0.selected } }
    func save(_ pairing: ApprovedServicePairing) throws {
        try value.withLock { try $0.upsert(pairing) }
        writes.withLock { $0 += 1 }
    }
    func select(_ id: ApprovedPairingID) throws { try value.withLock { try $0.select(id) } }
    func restoreSelection(_ previous: ApprovedPairingID, ifSelected attempted: ApprovedPairingID) throws {
        try value.withLock {
            guard $0.selectedID == attempted, $0.pairings.contains(where: { ApprovedPairingID($0) == previous }) else { return }
            try $0.select(previous)
        }
    }
    func delete() throws { value.withLock { $0 = ApprovedPairingCollection() } }
    func modify(_ change: (inout ApprovedPairingCollection) throws -> Void) throws {
        try value.withLock { current in
            var next = current
            try change(&next)
            current = next
        }
    }
}

private actor FakeRelay: HTTPTransport {
    enum ApprovalMode { case valid, wrongMacKey, wrongOwner, unsupportedProtocol }
    let mode: ApprovalMode
    var candidate: PairingCandidate?
    var requests: [URLRequest] = []

    init(mode: ApprovalMode = .valid) { self.mode = mode }

    func send(_ request: URLRequest) async throws -> HTTPReply {
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

    private func decryptedCandidateAgreement(_ envelope: Envelope) throws -> String {
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

    private func answer(_ status: Int, _ body: Data, _ request: URLRequest) -> HTTPReply {
        HTTPReply(status: status, body: body)
    }
}

@Test func submitsEncryptedCandidateAndAcceptsPinnedMacApproval() async throws {
    let relay = FakeRelay()
    let store = MemoryStore()
    let client = PairingClient(transport: relay, store: store,
        now: { Date(timeIntervalSince1970: Double(fixedNow) / 1_000) })
    let auth = try PairingAuthorization(bearerToken: "oidc-token", ownerSub: "owner-1")
    let pending = try await client.begin(qrPayload: invitation(), authorization: auth,
        displayName: "Alice’s iPhone")
    #expect(try store.load() == nil)
    let candidate = try #require(await relay.candidate)
    let publicKey = PairingWire.decode(candidate.signingKey)!
    #expect(pending.comparisonCode == PairingWire.encode(Data(SHA256.hash(data: publicKey).prefix(6))))
    #expect(candidate.envelope.kind == "pairing")
    #expect(candidate.envelope.expiresAtMilliseconds <= pending.expiresAtMs)
    #expect(!String(decoding: requestsBody(await relay.requests), as: UTF8.self).contains("the user’s iPhone"))
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

@Test func thePairingKeepsTheMacsIrohEndpointFromTheQR() async throws {
    let store = MemoryStore()
    let client = PairingClient(transport: FakeRelay(), store: store,
        now: { Date(timeIntervalSince1970: Double(fixedNow) / 1_000) })
    let auth = try PairingAuthorization(bearerToken: "oidc-token", ownerSub: "owner-1")
    let endpoint = String(repeating: "ab", count: 32)
    let pending = try await client.begin(qrPayload: invitation(irohEndpoint: endpoint), authorization: auth,
        displayName: "iPhone")
    #expect(try await client.waitForApproval(pending.attemptID).irohEndpoint == endpoint)
    #expect(try store.load()?.irohEndpoint == endpoint)
}

@Test func aQRWithAMalformedIrohEndpointIsRefused() throws {
    #expect(throws: PairingError.invalidInvitation) {
        try PairingInvitation(qrPayload: invitation(irohEndpoint: "not-a-key"))
    }
    #expect(try PairingInvitation(qrPayload: invitation()).macIrohEndpoint == nil)
}

@Test func theMacsIrohEndpointIsSavedOnceAndOnlyWhenValid() throws {
    let pairing = ApprovedServicePairing(relayURL: URL(string: "https://relay.example"), macID: "mac-1", deviceID: "phone-1",
        ownerSub: "owner-1", macSigningKey: "k", macAgreementKey: "a", signingPrivateKey: Data(), agreementPrivateKey: Data(),
        approvedAtMs: 1)
    var collection = ApprovedPairingCollection()
    try collection.upsert(pairing)
    let id = ApprovedPairingID(pairing)
    let endpoint = String(repeating: "cd", count: 32)
    let malformed = collection.updateIrohEndpoint(id, endpoint: "short")
    let learned = collection.updateIrohEndpoint(id, endpoint: endpoint)
    let again = collection.updateIrohEndpoint(id, endpoint: endpoint)
    #expect(!malformed)
    #expect(learned)
    #expect(!again)
    #expect(collection.selected?.irohEndpoint == endpoint)
    let decoded = try JSONDecoder().decode(ApprovedServicePairing.self, from: JSONEncoder().encode(pairing))
    #expect(decoded.irohEndpoint == nil, "a pairing saved before iroh still opens")
}

private func requestsBody(_ requests: [URLRequest]) -> Data {
    requests.first(where: { $0.url?.path.hasSuffix("/candidates") == true })?.httpBody ?? Data()
}

@Test(arguments: [FakeRelay.ApprovalMode.wrongMacKey, .wrongOwner])
private func rejectsForgedOrOtherAccountApproval(mode: FakeRelay.ApprovalMode) async throws {
    let store = MemoryStore()
    let client = PairingClient(transport: FakeRelay(mode: mode), store: store,
        now: { Date(timeIntervalSince1970: Double(fixedNow) / 1_000) })
    let auth = try PairingAuthorization(bearerToken: "oidc-token", ownerSub: "owner-1")
    let pending = try await client.begin(qrPayload: invitation(), authorization: auth,
        displayName: "iPhone")
    await #expect(throws: PairingError.invalidApproval) {
        try await client.waitForApproval(pending.attemptID)
    }
    #expect(try store.load() == nil)
}

@Test func rejectsExpiredInvitationBeforeSendingCandidate() async throws {
    let relay = FakeRelay()
    let client = PairingClient(transport: relay, store: MemoryStore(),
        now: { Date(timeIntervalSince1970: Double(fixedNow) / 1_000) })
    let auth = try PairingAuthorization(bearerToken: "oidc-token", ownerSub: "owner-1")
    await #expect(throws: PairingError.invalidInvitation) {
        try await client.begin(qrPayload: invitation(expiry: fixedNow + 1_000),
            authorization: auth, displayName: "iPhone")
    }
    #expect(await relay.requests.isEmpty)
}

@Test func rejectsUnsupportedProtocolBeforeSendingCandidate() async throws {
    let relay = FakeRelay(mode: .unsupportedProtocol)
    let store = MemoryStore()
    let client = PairingClient(transport: relay, store: store,
        now: { Date(timeIntervalSince1970: Double(fixedNow) / 1_000) })
    let auth = try PairingAuthorization(bearerToken: "oidc-token", ownerSub: "owner-1")
    await #expect(throws: RelayWireError.invalidResponse) {
        try await client.begin(qrPayload: invitation(), authorization: auth, displayName: "iPhone")
    }
    #expect(await relay.requests.map { $0.url?.path } == ["/v1/bootstrap"])
    #expect(await relay.candidate == nil)
    #expect(try store.load() == nil)
}

@Test func additionalMacRejectsExistingBindingBeforeNetworkOrStorageWrite() async throws {
    let store = MemoryStore(), relay = FakeRelay()
    let existing = try storedMac("mac-1", relay: "https://relay.example:443/")
    try store.save(existing)
    let client = PairingClient(transport: relay, store: store,
        now: { Date(timeIntervalSince1970: Double(fixedNow) / 1_000) })
    let auth = try PairingAuthorization(bearerToken: "oidc-token", ownerSub: "owner-1")
    await #expect(throws: PairingError.alreadyLinked) {
        try await client.begin(qrPayload: invitation(), authorization: auth, displayName: "Phone", rejectExistingMac: true)
    }
    #expect(await relay.requests.isEmpty) // No candidate, push registration or other HTTP.
    #expect(await relay.candidate == nil)
    #expect(store.writeCount == 1)
    #expect(try store.loadCollection().pairings.count == 1)
    #expect(try store.load()?.signingPrivateKey == existing.signingPrivateKey)
}

@Test func additionalDifferentMacPreservesExistingKeysThroughRealApprovalCodec() async throws {
    let store = MemoryStore(), relay = FakeRelay()
    let existing = try storedMac("mac-2")
    try store.save(existing)
    let client = PairingClient(transport: relay, store: store,
        now: { Date(timeIntervalSince1970: Double(fixedNow) / 1_000) })
    let auth = try PairingAuthorization(bearerToken: "oidc-token", ownerSub: "owner-1")
    let pending = try await client.begin(qrPayload: invitation(), authorization: auth, displayName: "Phone", rejectExistingMac: true)
    let approved = try await client.waitForApproval(pending.attemptID)
    let collection = try store.loadCollection()
    #expect(Set(collection.pairings.map(\.macID)) == ["mac-1", "mac-2"])
    #expect(collection.pairings.first { $0.macID == "mac-2" }?.signingPrivateKey == existing.signingPrivateKey)
    #expect(collection.selectedID == ApprovedPairingID(approved))
}

@Test func pairingARelayMacAgainReplacesItsRevokedPairing() async throws {
    let store = MemoryStore(), relay = FakeRelay()
    let revoked = try storedMac("mac-1", relay: "https://relay.example:443/")
    try store.save(revoked)
    let client = PairingClient(transport: relay, store: store,
        now: { Date(timeIntervalSince1970: Double(fixedNow) / 1_000) })
    let auth = try PairingAuthorization(bearerToken: "oidc-token", ownerSub: "owner-1")
    let pending = try await client.begin(qrPayload: invitation(), authorization: auth, displayName: "Phone",
                                         rejectExistingMac: true, replacing: "mac-1")
    #expect(await relay.candidate != nil)
    #expect(try store.loadCollection().pairings.map(\.deviceID) == ["existing-phone"], "The old pairing stays until approval")
    let approved = try await client.waitForApproval(pending.attemptID)
    let collection = try store.loadCollection()
    #expect(collection.pairings.map(\.deviceID) == [approved.deviceID])
    #expect(collection.selectedID == ApprovedPairingID(approved))
}

private func storedMac(_ mac: String, relay: String = "https://relay.example", owner: String = "owner-1") throws -> ApprovedServicePairing {
    ApprovedServicePairing(relayURL: URL(string: relay)!, macID: mac, deviceID: "existing-phone", ownerSub: owner,
        macSigningKey: PairingWire.encode(macSigning.publicKey.x963Representation),
        macAgreementKey: PairingWire.encode(macAgreement.publicKey.x963Representation),
        signingPrivateKey: P256.Signing.PrivateKey().rawRepresentation,
        agreementPrivateKey: P256.KeyAgreement.PrivateKey().rawRepresentation, approvedAtMs: fixedNow - 1000)
}

@Test(arguments: ["other-owner", "other-relay"])
func additionalMacDuplicateCheckUsesAccountAndRelay(_ differingScope: String) async throws {
    let store = MemoryStore(), relay = FakeRelay()
    try store.save(storedMac("mac-1", relay: differingScope == "other-relay" ? "https://other.example" : "https://relay.example",
        owner: differingScope == "other-owner" ? "owner-2" : "owner-1"))
    let client = PairingClient(transport: relay, store: store,
        now: { Date(timeIntervalSince1970: Double(fixedNow) / 1_000) })
    let pending = try await client.begin(qrPayload: invitation(),
        authorization: PairingAuthorization(bearerToken: "oidc-token", ownerSub: "owner-1"),
        displayName: "Phone", rejectExistingMac: true)
    #expect(await relay.candidate != nil)
    #expect(store.writeCount == 1)
    await client.discard(pending.attemptID)
}
