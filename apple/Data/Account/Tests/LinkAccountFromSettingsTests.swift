import AlondraTestSupport
import AlondraAccount
@testable import AlondraPairing
import AlondraTransport
import CryptoKit
import AlondraFeatures
import Foundation
import Synchronization
import Testing

private let macSigning = P256.Signing.PrivateKey()
private let macAgreement = P256.KeyAgreement.PrivateKey()
private let relayURL = "https://alondra-api.davidcollado.dev"
private let invitationExpiry = UInt64(Date().timeIntervalSince1970 * 1_000) + 600_000

private final class LoginNonce: Sendable {
    private let value = Mutex<String?>(nil)
    var current: String? { value.withLock { $0 } }
    func set(_ nonce: String) { value.withLock { $0 = nonce } }
}

private final class PermissionRequests: Sendable {
    private let value = Mutex(0)
    var count: Int { value.withLock { $0 } }
    func record() { value.withLock { $0 += 1 } }
}

private struct SigningInBrowser: AuthBrowser {
    let nonce: LoginNonce
    func authenticate(url: URL, callbackScheme: String, generation: Int) async throws -> URL {
        let items = URLComponents(url: url, resolvingAgainstBaseURL: false)!.queryItems!
        let values = Dictionary(uniqueKeysWithValues: items.map { ($0.name, $0.value!) })
        nonce.set(values["nonce"]!)
        return URL(string: "com.bitomule.alondra:/oauth2redirect/keycloak?code=auth-code&state=\(values["state"]!)")!
    }
    func cancelAuthentication(generation: Int) async {}
}

private struct KeycloakDouble: AuthHTTPTransport {
    let nonce: LoginNonce
    func send(_ request: URLRequest) async throws -> AuthHTTPResponse {
        let path = request.url!.path
        if path.hasSuffix("openid-configuration") {
            return AuthHTTPResponse(status: 200, body: Data("""
            {"issuer":"https://alondra-auth.davidcollado.dev/realms/alondra","authorization_endpoint":"https://alondra-auth.davidcollado.dev/realms/alondra/protocol/openid-connect/auth","token_endpoint":"https://alondra-auth.davidcollado.dev/realms/alondra/protocol/openid-connect/token","revocation_endpoint":"https://alondra-auth.davidcollado.dev/realms/alondra/protocol/openid-connect/revoke","userinfo_endpoint":"https://alondra-auth.davidcollado.dev/realms/alondra/protocol/openid-connect/userinfo"}
            """.utf8))
        }
        if path.hasSuffix("/revoke") { return AuthHTTPResponse(status: 200, body: Data()) }
        if path.hasSuffix("/userinfo") { return AuthHTTPResponse(status: 200, body: Data(#"{"sub":"owner-a"}"#.utf8)) }
        let claims = try JSONSerialization.data(withJSONObject: [
            "iss": "https://alondra-auth.davidcollado.dev/realms/alondra", "aud": "alondra-ios",
            "nonce": nonce.current!, "exp": Int(Date().timeIntervalSince1970) + 3_600])
        let body = try JSONSerialization.data(withJSONObject: [
            "access_token": "access", "refresh_token": "refresh", "token_type": "Bearer", "expires_in": 3_600,
            "id_token": "eyJhbGciOiJSUzI1NiJ9." + PairingWire.encode(claims) + ".signature"])
        return AuthHTTPResponse(status: 200, body: body)
    }
}

/// The relay, answering for a Mac that approves the first candidate it gets.
private actor RelayDouble: HTTPTransport {
    private var candidate: PairingCandidate?

    func send(_ request: URLRequest) async throws -> HTTPReply {
        let path = request.url!.path
        if path == "/v1/bootstrap" { return HTTPReply(status: 200, body: Data(#"{"protocol":1,"epoch":"BwcHBwcHBwcHBwcHBwcHBw"}"#.utf8)) }
        if path.hasSuffix("/candidates") {
            candidate = try JSONDecoder().decode(PairingCandidate.self, from: request.httpBody!)
            return HTTPReply(status: 202, body: Data())
        }
        guard path.hasSuffix("/approval"), let candidate else { return HTTPReply(status: 404, body: Data()) }
        let payload: [String: Any] = [
            "v": 1, "session_id": "pair-settings", "owner_sub": "owner-a", "mac_id": "mac-home",
            "mac_signing_key": PairingWire.encode(macSigning.publicKey.x963Representation),
            "mac_agreement_key": PairingWire.encode(macAgreement.publicKey.x963Representation),
            "device_id": candidate.deviceID, "phone_signing_key": candidate.signingKey,
            "phone_agreement_key": try Self.agreementKey(in: candidate.envelope),
            "expires_at_ms": invitationExpiry,
        ]
        let approval = try JSONDecoder().decode(PairingApproval.self, from: JSONSerialization.data(withJSONObject: payload))
        let proof: [String: Any] = [
            "approval": payload,
            "signature": PairingWire.encode(try macSigning.signature(for: approval.signedBytes()).rawRepresentation),
        ]
        return HTTPReply(status: 200, body: try JSONSerialization.data(withJSONObject: proof))
    }

    static func invitation() throws -> String {
        String(decoding: try JSONSerialization.data(withJSONObject: [
            "version": 1, "url": relayURL, "mac_id": "mac-home",
            "mac_signing_key": PairingWire.encode(macSigning.publicKey.x963Representation),
            "mac_agreement_key": PairingWire.encode(macAgreement.publicKey.x963Representation),
            "session_id": "pair-settings", "secret": PairingWire.encode(Data(repeating: 9, count: 16)),
            "expires_at_ms": invitationExpiry,
        ] as [String: Any]), as: UTF8.self)
    }

    private static func agreementKey(in envelope: Envelope) throws -> String {
        let ephemeral = try P256.KeyAgreement.PublicKey(x963Representation: PairingWire.decode(envelope.ephemeralKey)!)
        let nonce = PairingWire.decode(envelope.nonce)!
        var header = Data("alondra-e2e-envelope-v1".utf8)
        for value in [Data([1]), Data(envelope.route.utf8), Data(envelope.sender.utf8),
                      Data(envelope.recipient.utf8), Data(envelope.requestID.utf8),
                      Data(envelope.kind.utf8), PairingWire.bigEndian(envelope.expiresAtMilliseconds),
                      PairingWire.decode(envelope.ephemeralKey)!, nonce] {
            PairingWire.appendFrame(value, to: &header)
        }
        let key = try macAgreement.sharedSecretFromKeyAgreement(with: ephemeral)
            .hkdfDerivedSymmetricKey(using: SHA256.self, salt: Data(SHA256.hash(data: header)),
                                     sharedInfo: Data("alondra-e2e-key-v1".utf8), outputByteCount: 32)
        let sealed = try ChaChaPoly.SealedBox(combined: nonce + PairingWire.decode(envelope.ciphertext)!)
        let plain = try ChaChaPoly.open(sealed, using: key, authenticating: header)
        return (try JSONSerialization.jsonObject(with: plain) as! [String: String])["agreement_key"]!
    }
}

private func homeOnThisNetwork() -> ApprovedServicePairing {
    var local = ApprovedServicePairing(relayURL: nil, macID: "mac-home", deviceID: "phone-home", ownerSub: nil,
        macSigningKey: PairingWire.encode(macSigning.publicKey.x963Representation),
        macAgreementKey: PairingWire.encode(macAgreement.publicKey.x963Representation),
        signingPrivateKey: P256.Signing.PrivateKey().rawRepresentation,
        agreementPrivateKey: P256.KeyAgreement.PrivateKey().rawRepresentation, approvedAtMs: 1)
    local.name = "Studio"
    return local
}

@MainActor
@Suite(.temporaryFiles)
struct LinkAccountFromSettingsTests {
    @Test func anIPhoneWithOnlyALocalMacEndsLinkedThroughTheAccountWithAlertsToRegister() async throws {
        let nonce = LoginNonce()
        let auth = AuthSession(transport: KeycloakDouble(nonce: nonce), browser: SigningInBrowser(nonce: nonce),
                               storage: MemoryCredential(nil))
        var reconciled: [ApprovedServicePairing] = []
        let owner = AccountSession(auth: auth, displayName: "iPhone", storageDirectory: try TestFiles.directory(),
            didReconcile: { snapshot in
                reconciled = snapshot.entries.map(\.approved)
                return {}
            }, pairingStore: try MemoryPairings([homeOnThisNetwork()]),
            pairingTransport: RelayDouble(), localNetwork: MemoryLocalNetworkAccess().access,
            browse: { AsyncStream { $0.finish() } })
        try TestFiles.beforeRemoval { try await owner.stop() }
        let requests = PermissionRequests()
        owner.alerts = SignInClient.Alerts(permission: { .notDetermined }, request: { requests.record() }, openSettings: {})
        await owner.store.send(.appeared).finish()
        #expect(owner.macs.map(\.local) == [true])
        #expect(owner.store.phase == .approved)

        let settings = owner.additionalMacPairing
        try await settings.login()
        #expect(owner.macs.map(\.local) == [true], "Signing in keeps the Mac paired on this network")
        _ = try await settings.begin(try RelayDouble.invitation())
        try await settings.approve()

        #expect(owner.macs.map(\.macID) == ["mac-home"])
        #expect(owner.macs.map(\.local) == [false])
        #expect(owner.services != nil)
        #expect(reconciled.map(\.macID) == ["mac-home"])
        #expect(reconciled.first?.relayURL == URL(string: relayURL))
        #expect(reconciled.first?.ownerSub == "owner-a")
        #expect(owner.store.phase == .alerts(.notDetermined), "Settings closes with its workspace, so the first screen offers alerts")
        #expect(owner.store.finishingPairing)
        #expect(requests.count == 0, "Alerts are asked for only from the step")
        await owner.store.send(.turnOnAlerts).finish()
        #expect(requests.count == 1)
        #expect(owner.store.phase == .approved)
    }
}
