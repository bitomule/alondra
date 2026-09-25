import AlondraShared
import AlondraDeviceIdentity
import Foundation
import Observation
import UIKit

struct PairingInvite: Equatable {
    let url: URL
    let code: String

    init?(url: String, code: String) {
        let code = code.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let url = AlondraClient.serverURL(url), !code.isEmpty else { return nil }
        self.url = url
        self.code = code
    }

    init?(qrPayload: String) {
        struct Payload: Decodable {
            let url: String
            let code: String
        }
        guard let payload = try? JSONDecoder().decode(Payload.self, from: Data(qrPayload.utf8)) else { return nil }
        self.init(url: payload.url, code: payload.code)
    }
}

@MainActor
@Observable
final class DevicePairing {
    /// Changes even when a server keeps the same device ID after key rotation.
    private(set) var revision = UUID()
    private(set) var deviceID: String?
    private(set) var pairedURL: URL?
    private(set) var signer: AlondraDeviceIdentity.DeviceSigner?
    private let identities: DeviceIdentityStore

    init(identities: DeviceIdentityStore = DeviceIdentityStore()) {
        self.identities = identities
        guard let identity = try? identities.pairedIOSIdentity() else { return }
        deviceID = identity.signer.deviceID
        pairedURL = identity.serverURL
        signer = identity.signer
        revision = UUID()
    }

    var isPaired: Bool { signer != nil }
    var credentialRevision: String? { signer?.key.publicKey.x963Representation.base64EncodedString() }

    /// A signer belongs to exactly one server. Use this for all client construction,
    /// especially when Settings tests a candidate URL before switching servers.
    func signer(for serverURL: URL) -> AlondraDeviceIdentity.DeviceSigner? {
        guard let identity = pairedURL.flatMap({ url in
            signer.map { DeviceIdentity(serverURL: url, signer: $0) }
        }), identity.matches(serverURL: serverURL) else { return nil }
        return identity.signer
    }

    func pair(_ invite: PairingInvite) async throws {
        let previous = try identities.pairedIOSIdentity()
        let key = try identities.beginIOSPairing()
        let id: String
        do {
            id = try await AlondraClient.pair(
                at: invite.url,
                code: invite.code,
                deviceName: UIDevice.current.name,
                publicKey: key.publicKey.x963Representation
            )
        } catch {
            try identities.restoreIOSKey(previous)
            throw error
        }
        let identity = identities.completeIOSPairing(serverURL: invite.url, deviceID: id, key: key)
        deviceID = identity.signer.deviceID
        pairedURL = identity.serverURL
        signer = identity.signer
        revision = UUID()
    }

    func unpair() {
        identities.unpairIOS()
        deviceID = nil
        pairedURL = nil
        signer = nil
        revision = UUID()
    }
}
