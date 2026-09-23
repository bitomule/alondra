import AlondraShared
import Foundation
import Observation

/// This Mac's paired identity on the local alondrad, so every request it sends is signed
/// and answers reach nodes as coming from a verified device.
@MainActor
@Observable
final class LocalDevice {
    private(set) var client: AlondraClient

    private let base: URL
    private static let serverKey = "serverURL"
    private static let deviceIDsKey = "localDeviceIDs"

    init(defaults: UserDefaults = .standard) {
        base = defaults.string(forKey: Self.serverKey).flatMap(AlondraClient.serverURL) ?? AlondraClient.defaultURL
        client = AlondraClient(base: base)
        if let id = Self.deviceIDs(defaults)[base.absoluteString], let key = try? DeviceKeyStore.load() {
            client = AlondraClient(base: base, signer: DeviceSigner(deviceID: id, key: key))
        }
    }

    var isPaired: Bool { client.signer != nil }

    /// `/v1/pairing` only answers loopback callers, so the Mac can pair itself without asking David.
    func pair() async throws {
        let key = try Self.signingKey()
        let invite = try await AlondraClient(base: base).createPairing()
        let id = try await AlondraClient.pair(
            at: base,
            code: invite.code,
            deviceName: "Mac (\(Host.current().localizedName ?? ProcessInfo.processInfo.hostName))",
            publicKey: key.publicKey.x963Representation
        )
        var ids = Self.deviceIDs(.standard)
        ids[base.absoluteString] = id
        UserDefaults.standard.set(ids, forKey: Self.deviceIDsKey)
        client = AlondraClient(base: base, signer: DeviceSigner(deviceID: id, key: key))
    }

    private static func signingKey() throws -> DeviceSigningKey {
        if let key = try? DeviceKeyStore.load() { return key }
        let key = DeviceSigningKey.generate()
        try DeviceKeyStore.save(key)
        return key
    }

    private static func deviceIDs(_ defaults: UserDefaults) -> [String: String] {
        defaults.dictionary(forKey: deviceIDsKey) as? [String: String] ?? [:]
    }
}
