import AlondraShared
import AlondraDeviceIdentity
import Foundation
import Observation

/// This Mac's paired identity on the local alondrad, so every request it sends is signed
/// and answers reach nodes as coming from a verified device.
@MainActor
@Observable
final class LocalDevice {
    private(set) var client: AlondraClient

    private let base: URL
    private let identities: DeviceIdentityStore
    private static let serverKey = "serverURL"

    init(defaults: UserDefaults = .standard, keys: any DeviceKeyStorage = SystemDeviceKeyStorage()) {
        base = defaults.string(forKey: Self.serverKey).flatMap(AlondraClient.serverURL) ?? AlondraClient.defaultURL
        identities = DeviceIdentityStore(defaults: defaults, keys: keys)
        client = AlondraClient(base: base)
        if let identity = try? identities.pairedMacIdentity(at: base) {
            client = AlondraClient(base: base, signer: identity.signer)
        }
    }

    var isPaired: Bool { client.signer != nil }

    /// `/v1/pairing` only answers loopback callers, so the Mac can pair itself without asking David.
    func pair() async throws {
        let key = try identities.macSigningKey()
        let invite = try await AlondraClient(base: base).createPairing()
        let id = try await AlondraClient.pair(
            at: base,
            code: invite.code,
            deviceName: "Mac (\(Host.current().localizedName ?? ProcessInfo.processInfo.hostName))",
            publicKey: key.publicKey.x963Representation
        )
        let identity = identities.completeMacPairing(at: base, deviceID: id, key: key)
        client = AlondraClient(base: base, signer: identity.signer)
    }
}
