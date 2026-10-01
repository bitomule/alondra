import CryptoKit
import Foundation
import AlondraTransport

/// Implements the iPhone half of the Mac-owned, encrypted service pairing handshake.
/// The caller supplies identity only after OIDC has verified the bearer and subject.
public actor PairingClient {
    private struct Attempt {
        let invitation: PairingInvitation
        let authorization: PairingAuthorization
        let deviceID: String
        let signingPrivateKey: Data
        let agreementPrivateKey: Data
        let signingPublicKey: String
        let agreementPublicKey: String
    }

    private let transport: any HTTPTransport
    private let store: any ApprovedPairingStore
    private let now: @Sendable () -> Date
    private var attempts: [UUID: Attempt] = [:]

    public init(
        transport: (any HTTPTransport)? = nil,
        store: any ApprovedPairingStore = KeychainApprovedPairingStore(),
        now: @escaping @Sendable () -> Date = { Date() }
    ) {
        self.transport = transport ?? AlondraAPIClient.makeHTTPTransport()
        self.store = store
        self.now = now
    }

    public func approvedPairings() throws -> ApprovedPairingCollection { try store.loadCollection() }
    public func selectApprovedPairing(_ id: ApprovedPairingID) throws { try store.select(id) }
    /// Synchronous compensation lets a MainActor owner check its generation and
    /// restore the preference without an actor hop after logout validation.
    public nonisolated func restoreApprovedSelection(_ previous: ApprovedPairingID, ifSelected attempted: ApprovedPairingID) throws {
        try store.restoreSelection(previous, ifSelected: attempted)
    }
    public func discardAllAttempts() { attempts.removeAll() }

    public func approvedPairing() throws -> ApprovedServicePairing? { try store.load() }

    public func removeApprovedPairing() throws {
        attempts.removeAll()
        try store.delete()
    }

    /// Submit a candidate. The returned code must remain visible until the Mac owner types it.
    public func begin(
        qrPayload: String, authorization: PairingAuthorization, displayName: String,
        rejectExistingMac: Bool = false
    ) async throws -> PairingPending {
        let invitation = try PairingInvitation(qrPayload: qrPayload)
        let start = currentMs()
        try Task.checkCancellation()
        guard invitation.expiresAtMs > start + 5_000,
              !displayName.isEmpty, displayName.utf8.count <= 80,
              !displayName.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains) else {
            throw PairingError.invalidInvitation
        }
        if rejectExistingMac, try store.loadCollection().pairings.contains(where: {
            $0.ownerSub == authorization.ownerSub && $0.macID == invitation.macID &&
                Self.sameRelay($0.relayURL, invitation.url)
        }) { throw PairingError.alreadyLinked }
        let signing = P256.Signing.PrivateKey()
        let agreement = P256.KeyAgreement.PrivateKey()
        let signingBytes = signing.publicKey.x963Representation
        let signingPublic = PairingWire.encode(signingBytes)
        let agreementPublic = PairingWire.encode(agreement.publicKey.x963Representation)
        let deviceID = "phone-" + PairingWire.encode(try PairingWire.random(16))
        let comparisonCode = PairingWire.encode(Data(SHA256.hash(data: signingBytes).prefix(6)))
        let expiry = min(start + 600_000, invitation.expiresAtMs)
        guard let macAgreement = PairingWire.decode(invitation.macAgreementKey) else {
            throw PairingError.invalidInvitation
        }
        let recipient = try P256.KeyAgreement.PublicKey(x963Representation: macAgreement)
        let plain = PairingCandidatePlain(deviceID: deviceID, signingKey: signingPublic,
            agreementKey: agreementPublic, displayName: displayName,
            ownerSub: authorization.ownerSub, sessionID: invitation.sessionID)
        let envelope = try EnvelopeV1.seal(
            route: invitation.macID, sender: deviceID, recipient: invitation.macID,
            requestID: invitation.sessionID, kind: "pairing",
            expiresAtMilliseconds: expiry, plaintext: JSONEncoder().encode(plain),
            senderKey: signing, recipientKey: recipient)
        let candidate = PairingCandidate(secret: invitation.secret, deviceID: deviceID,
            signingKey: signingPublic, envelope: envelope)
        let body = try JSONEncoder().encode(candidate)
        let connection = try connection(at: invitation.url, deviceID: deviceID, signing: signing, authorization: authorization)
        let route = "/v1/pairings/\(invitation.sessionID)/candidates"
        let response = try await connection.pairingRequest(method: "POST", target: route, body: body)
        try Task.checkCancellation()
        guard response.status == 202 else { throw PairingError.unexpectedStatus(response.status) }
        let id = UUID()
        attempts[id] = Attempt(invitation: invitation, authorization: authorization,
            deviceID: deviceID, signingPrivateKey: signing.rawRepresentation,
            agreementPrivateKey: agreement.rawRepresentation,
            signingPublicKey: signingPublic, agreementPublicKey: agreementPublic)
        return PairingPending(attemptID: id, comparisonCode: comparisonCode,
            expiresAtMs: invitation.expiresAtMs)
    }

    private static func sameRelay(_ lhs: URL, _ rhs: URL) -> Bool {
        func normalized(_ url: URL) -> URLComponents? {
            guard var parts = URLComponents(url: url, resolvingAgainstBaseURL: false) else { return nil }
            parts.scheme = parts.scheme?.lowercased(); parts.host = parts.host?.lowercased()
            if parts.scheme == "https", parts.port == 443 { parts.port = nil }
            while parts.path.hasSuffix("/") { parts.path.removeLast() }
            return parts
        }
        return normalized(lhs) == normalized(rhs)
    }

    /// Polls until the Mac approves or the QR expires. Cancellation stops the poll without saving keys.
    public func waitForApproval(_ id: UUID) async throws -> ApprovedServicePairing {
        guard let attempt = attempts[id] else { throw PairingError.missingAttempt }
        let signing = try P256.Signing.PrivateKey(rawRepresentation: attempt.signingPrivateKey)
        let route = "/v1/pairings/\(attempt.invitation.sessionID)/approval"
        let connection = try connection(at: attempt.invitation.url, deviceID: attempt.deviceID,
            signing: signing, authorization: attempt.authorization)
        while true {
            try Task.checkCancellation()
            guard currentMs() < attempt.invitation.expiresAtMs else {
                attempts.removeValue(forKey: id)
                throw PairingError.expired
            }
            _ = try await connection.bootstrap()
            let response = try await connection.pairingRequest(method: "GET", target: route, body: Data())
            let body = response.body
            switch response.status {
            case 204:
                try await Task.sleep(for: .seconds(2))
            case 200:
                let proof = try JSONDecoder().decode(PairingApprovalProof.self, from: body)
                try proof.verify(invitation: attempt.invitation,
                    authorization: attempt.authorization, deviceID: attempt.deviceID,
                    signingKey: attempt.signingPublicKey,
                    agreementKey: attempt.agreementPublicKey, nowMs: currentMs())
                try Task.checkCancellation()
                guard attempts[id] != nil else { throw PairingError.missingAttempt }
                let approved = ApprovedServicePairing(relayURL: attempt.invitation.url,
                    macID: attempt.invitation.macID, deviceID: attempt.deviceID,
                    ownerSub: attempt.authorization.ownerSub,
                    macSigningKey: attempt.invitation.macSigningKey,
                    macAgreementKey: attempt.invitation.macAgreementKey,
                    signingPrivateKey: attempt.signingPrivateKey,
                    agreementPrivateKey: attempt.agreementPrivateKey,
                    approvedAtMs: currentMs())
                try store.save(approved)
                attempts.removeValue(forKey: id)
                return approved
            case 404:
                attempts.removeValue(forKey: id)
                throw PairingError.expired
            default:
                throw PairingError.unexpectedStatus(response.status)
            }
        }
    }

    public func discard(_ id: UUID) { attempts.removeValue(forKey: id) }

    private func currentMs() -> UInt64 { UInt64(max(0, now().timeIntervalSince1970 * 1_000)) }

    private func connection(at base: URL, deviceID: String, signing: P256.Signing.PrivateKey,
                            authorization: PairingAuthorization) throws -> any Transport {
        let now = self.now
        return try AlondraAPIClient.makeTransport(base: base, deviceID: deviceID, signing: signing,
            bearer: { authorization.bearerToken }, transport: transport,
            now: { UInt64(max(0, now().timeIntervalSince1970 * 1_000)) })
    }
}
