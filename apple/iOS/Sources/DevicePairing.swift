import AlondraShared
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
    private(set) var deviceID: String?
    private(set) var pairedURL: URL?
    private(set) var signer: DeviceSigner?

    private static let deviceIDKey = "pairedDeviceID"
    private static let pairedURLKey = "pairedURL"

    init() {
        let defaults = UserDefaults.standard
        guard let id = defaults.string(forKey: Self.deviceIDKey),
              let url = defaults.url(forKey: Self.pairedURLKey),
              let key = try? DeviceKeyStore.load() else { return }
        deviceID = id
        pairedURL = url
        signer = DeviceSigner(deviceID: id, key: key)
    }

    var isPaired: Bool { signer != nil }

    func pair(_ invite: PairingInvite) async throws {
        let key = DeviceSigningKey.generate()
        try DeviceKeyStore.save(key)
        let id: String
        do {
            id = try await AlondraClient.pair(
                at: invite.url,
                code: invite.code,
                deviceName: UIDevice.current.name,
                publicKey: key.publicKey.x963Representation
            )
        } catch {
            if let previous = signer {
                try? DeviceKeyStore.save(previous.key)
            } else {
                DeviceKeyStore.delete()
            }
            throw error
        }
        let defaults = UserDefaults.standard
        defaults.set(id, forKey: Self.deviceIDKey)
        defaults.set(invite.url, forKey: Self.pairedURLKey)
        defaults.set(invite.url.absoluteString, forKey: ServerSetting.key)
        deviceID = id
        pairedURL = invite.url
        signer = DeviceSigner(deviceID: id, key: key)
    }

    func unpair() {
        DeviceKeyStore.delete()
        let defaults = UserDefaults.standard
        defaults.removeObject(forKey: Self.deviceIDKey)
        defaults.removeObject(forKey: Self.pairedURLKey)
        deviceID = nil
        pairedURL = nil
        signer = nil
    }
}
