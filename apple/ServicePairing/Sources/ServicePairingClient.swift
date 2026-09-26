import CryptoKit
import Foundation
import AlondraRelayClient

/// Implements the iPhone half of the Mac-owned, encrypted service pairing handshake.
/// The caller supplies identity only after OIDC has verified the bearer and subject.
public actor ServicePairingClient {
    private struct Attempt {
        let invitation: ServicePairingInvitation
        let authorization: PairingAuthorization
        let deviceID: String
        let signingPrivateKey: Data
        let agreementPrivateKey: Data
        let signingPublicKey: String
        let agreementPublicKey: String
    }

    private let transport: any PairingTransport
    private let store: any ApprovedPairingStore
    private let now: @Sendable () -> Date
    private var attempts: [UUID: Attempt] = [:]

    public init(
        transport: any PairingTransport = URLSessionPairingTransport(),
        store: any ApprovedPairingStore = KeychainApprovedPairingStore(),
        now: @escaping @Sendable () -> Date = { Date() }
    ) {
        self.transport = transport
        self.store = store
        self.now = now
    }

    public func approvedPairing() throws -> ApprovedServicePairing? { try store.load() }

    public func removeApprovedPairing() throws {
        attempts.removeAll()
        try store.delete()
    }

    /// Submit a candidate. The returned code must remain visible until the Mac owner types it.
    public func begin(
        qrPayload: String, authorization: PairingAuthorization, displayName: String
    ) async throws -> PairingPending {
        let invitation = try ServicePairingInvitation(qrPayload: qrPayload)
        let start = currentMs()
        try Task.checkCancellation()
        guard invitation.expiresAtMs > start + 5_000,
              !displayName.isEmpty, displayName.utf8.count <= 80,
              !displayName.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains) else {
            throw ServicePairingError.invalidInvitation
        }
        let signing = P256.Signing.PrivateKey()
        let agreement = P256.KeyAgreement.PrivateKey()
        let signingBytes = signing.publicKey.x963Representation
        let signingPublic = PairingWire.encode(signingBytes)
        let agreementPublic = PairingWire.encode(agreement.publicKey.x963Representation)
        let deviceID = "phone-" + PairingWire.encode(try PairingWire.random(16))
        let comparisonCode = PairingWire.encode(Data(SHA256.hash(data: signingBytes).prefix(6)))
        let expiry = min(start + 600_000, invitation.expiresAtMs)
        guard let macAgreement = PairingWire.decode(invitation.macAgreementKey) else {
            throw ServicePairingError.invalidInvitation
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
        let epoch = try await bootstrap(at: invitation.url)
        let route = "/v1/pairings/\(invitation.sessionID)/candidates"
        let response = try await request(at: invitation.url, method: "POST", route: route,
            body: body, epoch: epoch, deviceID: deviceID, signer: signing,
            bearer: authorization.bearerToken)
        try Task.checkCancellation()
        guard response.statusCode == 202 else { throw ServicePairingError.unexpectedStatus(response.statusCode) }
        let id = UUID()
        attempts[id] = Attempt(invitation: invitation, authorization: authorization,
            deviceID: deviceID, signingPrivateKey: signing.rawRepresentation,
            agreementPrivateKey: agreement.rawRepresentation,
            signingPublicKey: signingPublic, agreementPublicKey: agreementPublic)
        return PairingPending(attemptID: id, comparisonCode: comparisonCode,
            expiresAtMs: invitation.expiresAtMs)
    }

    /// Polls until the Mac approves or the QR expires. Cancellation stops the poll without saving keys.
    public func waitForApproval(_ id: UUID) async throws -> ApprovedServicePairing {
        guard let attempt = attempts[id] else { throw ServicePairingError.missingAttempt }
        let signing = try P256.Signing.PrivateKey(rawRepresentation: attempt.signingPrivateKey)
        let route = "/v1/pairings/\(attempt.invitation.sessionID)/approval"
        while true {
            try Task.checkCancellation()
            guard currentMs() < attempt.invitation.expiresAtMs else {
                attempts.removeValue(forKey: id)
                throw ServicePairingError.expired
            }
            let epoch = try await bootstrap(at: attempt.invitation.url)
            let (body, response) = try await requestBody(at: attempt.invitation.url,
                method: "GET", route: route, body: Data(), epoch: epoch,
                deviceID: attempt.deviceID, signer: signing,
                bearer: attempt.authorization.bearerToken)
            switch response.statusCode {
            case 204:
                try await Task.sleep(for: .seconds(2))
            case 200:
                let proof = try JSONDecoder().decode(PairingApprovalProof.self, from: body)
                try proof.verify(invitation: attempt.invitation,
                    authorization: attempt.authorization, deviceID: attempt.deviceID,
                    signingKey: attempt.signingPublicKey,
                    agreementKey: attempt.agreementPublicKey, nowMs: currentMs())
                try Task.checkCancellation()
                guard attempts[id] != nil else { throw ServicePairingError.missingAttempt }
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
                throw ServicePairingError.expired
            default:
                throw ServicePairingError.unexpectedStatus(response.statusCode)
            }
        }
    }

    public func discard(_ id: UUID) { attempts.removeValue(forKey: id) }

    private func currentMs() -> UInt64 { UInt64(max(0, now().timeIntervalSince1970 * 1_000)) }

    private func bootstrap(at base: URL) async throws -> Data {
        var request = URLRequest(url: base.appending(path: "v1/bootstrap"))
        request.httpMethod = "GET"
        request.timeoutInterval = 15
        let (data, response) = try await transport.send(request)
        guard response.statusCode == 200 else { throw ServicePairingError.unexpectedStatus(response.statusCode) }
        struct Bootstrap: Decodable { let epoch: String }
        let epoch = try JSONDecoder().decode(Bootstrap.self, from: data)
        guard let bytes = PairingWire.decode(epoch.epoch), bytes.count == 16 else {
            throw ServicePairingError.invalidResponse
        }
        return bytes
    }

    private func request(
        at base: URL, method: String, route: String, body: Data, epoch: Data,
        deviceID: String, signer: P256.Signing.PrivateKey, bearer: String
    ) async throws -> HTTPURLResponse {
        let (_, response) = try await requestBody(at: base, method: method, route: route,
            body: body, epoch: epoch, deviceID: deviceID, signer: signer, bearer: bearer)
        return response
    }

    private func requestBody(
        at base: URL, method: String, route: String, body: Data, epoch: Data,
        deviceID: String, signer: P256.Signing.PrivateKey, bearer: String
    ) async throws -> (Data, HTTPURLResponse) {
        guard let url = URL(string: route, relativeTo: base)?.absoluteURL,
              url.host() == base.host(), url.scheme == base.scheme else {
            throw ServicePairingError.invalidInvitation
        }
        let timestamp = currentMs()
        let nonce = try PairingWire.random(16)
        let challenge = try RelayHTTPChallenge.message(deviceID: deviceID, method: method,
            target: route, epoch: epoch, body: body,
            timestampMilliseconds: timestamp, nonce: nonce)
        let signature = try signer.signature(for: challenge).rawRepresentation
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.timeoutInterval = 15
        if method == "POST" {
            request.httpBody = body
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        request.setValue("Bearer \(bearer)", forHTTPHeaderField: "Authorization")
        request.setValue(deviceID, forHTTPHeaderField: "X-Device-ID")
        request.setValue(String(timestamp), forHTTPHeaderField: "X-Device-Timestamp-Ms")
        request.setValue(PairingWire.encode(nonce), forHTTPHeaderField: "X-Device-Nonce")
        request.setValue(PairingWire.encode(signature), forHTTPHeaderField: "X-Device-Signature")
        request.setValue(PairingWire.encode(epoch), forHTTPHeaderField: "X-Relay-Epoch")
        return try await transport.send(request)
    }
}
