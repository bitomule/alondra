import AlondraTransport
import CryptoKit
import Foundation

/// One direct pairing attempt, up to the code both screens show.
public struct DirectPairingAttempt: Sendable, Equatable {
    public let id: UUID
    public let code: String
    public let macName: String
    public let expiresAtMs: UInt64
}

/// The device half of pairing on the local network (`docs/direct.md`): fresh keys per Mac,
/// commit, reveal, show the code, then wait for the Mac's signed approval and keep it.
public actor DirectPairingClient {
    private struct Attempt {
        let endpoint: DirectEndpoint
        let hello: DirectMacIdentity
        let transcript: DirectPairingProtocol.Transcript
        let signing: P256.Signing.PrivateKey
        let agreement: P256.KeyAgreement.PrivateKey
        let expiresAtMs: UInt64
    }

    private struct Started: Decodable {
        let pairingID: String
        let macNonce: String
        let expiresAtMs: UInt64
        enum CodingKeys: String, CodingKey {
            case pairingID = "pairing_id", macNonce = "mac_nonce", expiresAtMs = "expires_at_ms"
        }
    }

    private struct Status: Decodable {
        let state: String
        let approval: DirectPairingProtocol.Approval?
        let signature: String?
    }

    private let http: DirectHTTP
    private let store: any ApprovedPairingStore
    private let devicePrefix: String
    private let now: @Sendable () -> UInt64
    private let pause: @Sendable () async throws -> Void
    private var attempts: [UUID: Attempt] = [:]

    public init(transport: (any HTTPTransport)? = nil,
                store: any ApprovedPairingStore = KeychainApprovedPairingStore(),
                devicePrefix: String,
                now: @escaping @Sendable () -> UInt64 = { UInt64(max(0, Date().timeIntervalSince1970 * 1000)) },
                pause: @escaping @Sendable () async throws -> Void = { try await Task.sleep(for: .seconds(1)) }) {
        http = DirectHTTP(transport: transport ?? AlondraAPIClient.makeHTTPTransport())
        self.store = store
        self.devicePrefix = devicePrefix
        self.now = now
        self.pause = pause
    }

    /// `excluding` is the Mac this app runs on: it cannot pair with itself.
    public func begin(at endpoint: DirectEndpoint, displayName: String, excluding ownMacID: String? = nil) async throws -> DirectPairingAttempt {
        guard !displayName.isEmpty, displayName.utf8.count <= 80,
              !displayName.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains) else {
            throw PairingError.invalidIdentity
        }
        let hello: DirectMacIdentity
        do { hello = try await http.hello(at: endpoint) }
        catch let error as URLError where error.code == .appTransportSecurityRequiresSecureConnection {
            throw PairingError.insecureAddress
        } catch is URLError { throw PairingError.macUnreachable }
        if hello.macID == ownMacID { throw PairingError.thisMac }
        if try store.loadCollection().pairings.contains(where: { $0.macID == hello.macID }) {
            throw PairingError.alreadyLinked
        }
        let macAgreement = try hello.agreementPublicKey()
        let signing = P256.Signing.PrivateKey()
        let agreement = P256.KeyAgreement.PrivateKey()
        let deviceID = devicePrefix + "-" + (try PairingWire.random(8)).map { String(format: "%02x", $0) }.joined()
        let signingKey = PairingWire.encode(signing.publicKey.x963Representation)
        let agreementKey = PairingWire.encode(agreement.publicKey.x963Representation)
        let clientNonce = PairingWire.encode(try PairingWire.random(DirectPairingProtocol.nonceLength))
        let commitment = try DirectPairingProtocol.commitment(clientNonce: clientNonce, deviceID: deviceID,
                                                              signingKey: signingKey, agreementKey: agreementKey)
        let opening = try seal(["device_id": deviceID, "name": displayName, "signing_key": signingKey,
                                "agreement_key": agreementKey, "commitment": commitment],
                               macID: hello.macID, deviceID: deviceID, signing: signing, macAgreement: macAgreement)
        let startBody = try JSONSerialization.data(withJSONObject: ["signing_key": signingKey, "envelope": opening])
        let started: Started = try decode(try await post(endpoint, "/direct/pairing", startBody))
        let reveal = try seal(["pairing_id": started.pairingID, "client_nonce": clientNonce],
                              macID: hello.macID, deviceID: deviceID, signing: signing, macAgreement: macAgreement)
        _ = try await post(endpoint, "/direct/pairing/\(started.pairingID)/reveal",
                           try JSONSerialization.data(withJSONObject: ["envelope": reveal]))
        let transcript = DirectPairingProtocol.Transcript(
            pairingID: started.pairingID, macID: hello.macID, macSigningKey: hello.signingKey,
            macAgreementKey: hello.agreementKey, deviceID: deviceID, clientSigningKey: signingKey,
            clientAgreementKey: agreementKey, clientNonce: clientNonce, macNonce: started.macNonce)
        let id = UUID()
        attempts[id] = Attempt(endpoint: endpoint, hello: hello, transcript: transcript, signing: signing,
                               agreement: agreement, expiresAtMs: started.expiresAtMs)
        return DirectPairingAttempt(id: id, code: try transcript.code(), macName: hello.name, expiresAtMs: started.expiresAtMs)
    }

    /// Polls until someone on the Mac decides, verifies the signed approval and saves the pairing.
    public func waitForApproval(_ id: UUID) async throws -> ApprovedServicePairing {
        guard let attempt = attempts[id] else { throw PairingError.missingAttempt }
        defer { attempts[id] = nil }
        let path = "/direct/pairing/\(attempt.transcript.pairingID)"
        while true {
            try Task.checkCancellation()
            guard now() < attempt.expiresAtMs else { throw PairingError.requestExpired }
            let reply: HTTPReply
            do { reply = try await http.get(attempt.endpoint, path: path, timeout: .seconds(5)) }
            catch is URLError { try await pause(); continue }
            let status: Status = try decode(try checked(reply))
            switch status.state {
            case "approved":
                guard let approval = status.approval, let signature = status.signature else { throw PairingError.invalidApproval }
                try approval.verify(signature: signature, macKey: attempt.hello.signingPublicKey(),
                                    expected: attempt.transcript, nowMs: now())
                var approved = ApprovedServicePairing(
                    relayURL: nil, macID: attempt.hello.macID, deviceID: attempt.transcript.deviceID, ownerSub: nil,
                    macSigningKey: attempt.hello.signingKey, macAgreementKey: attempt.hello.agreementKey,
                    signingPrivateKey: attempt.signing.rawRepresentation,
                    agreementPrivateKey: attempt.agreement.rawRepresentation, approvedAtMs: now())
                approved.name = attempt.hello.name
                approved.routes = [attempt.endpoint]
                try Task.checkCancellation()
                try store.save(approved)
                LocalNetworkAccess.record(.allowed)
                return approved
            case "denied": throw PairingError.denied
            case "expired": throw PairingError.requestExpired
            default: try await pause()
            }
        }
    }

    public func discard(_ id: UUID) { attempts[id] = nil }

    private func seal(_ plain: [String: String], macID: String, deviceID: String,
                      signing: P256.Signing.PrivateKey, macAgreement: P256.KeyAgreement.PublicKey) throws -> Any {
        let envelope = try Envelope.seal(
            route: macID, sender: deviceID, recipient: macID,
            requestID: "pair-" + UUID().uuidString.lowercased(), kind: "pairing",
            expiresAtMilliseconds: now() + 120_000,
            plaintext: try JSONSerialization.data(withJSONObject: plain, options: [.sortedKeys]),
            senderKey: signing, recipientKey: macAgreement)
        return try JSONSerialization.jsonObject(with: JSONEncoder().encode(envelope))
    }

    private func post(_ endpoint: DirectEndpoint, _ path: String, _ body: Data) async throws -> Data {
        do { return try checked(try await http.post(endpoint, path: path, body: body, timeout: .seconds(10))) }
        catch is URLError { throw PairingError.macUnreachable }
    }

    private func checked(_ reply: HTTPReply) throws -> Data {
        switch reply.status {
        case 200: return reply.body
        case 403: throw PairingError.denied
        case 410: throw PairingError.requestExpired
        case 429: throw PairingError.busy
        default: throw PairingError.unexpectedStatus(reply.status)
        }
    }

    private func decode<T: Decodable>(_ data: Data) throws -> T {
        do { return try JSONDecoder().decode(T.self, from: data) }
        catch { throw PairingError.invalidResponse }
    }
}
