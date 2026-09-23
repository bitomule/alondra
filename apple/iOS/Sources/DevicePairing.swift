import AlondraShared
import CryptoKit
import Foundation
import Observation
import Security
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
              let key = try? KeyStore.load() else { return }
        deviceID = id
        pairedURL = url
        signer = DeviceSigner(deviceID: id, key: key)
    }

    var isPaired: Bool { signer != nil }

    func pair(_ invite: PairingInvite) async throws {
        let key = try DeviceSigningKey.generate()
        try KeyStore.save(key)
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
                try? KeyStore.save(previous.key)
            } else {
                KeyStore.delete()
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
        KeyStore.delete()
        let defaults = UserDefaults.standard
        defaults.removeObject(forKey: Self.deviceIDKey)
        defaults.removeObject(forKey: Self.pairedURLKey)
        deviceID = nil
        pairedURL = nil
        signer = nil
    }
}

enum KeyStore {
    private static let service = "com.bitomule.alondra.signing-key"
    private static let secureEnclaveAccount = "secure-enclave"
    private static let softwareAccount = "software"

    enum Failure: LocalizedError {
        case keychain(OSStatus)

        var errorDescription: String? {
            switch self {
            case .keychain(let status):
                String(localized: "Couldn’t save the key to the keychain (\(SecCopyErrorMessageString(status, nil) as String? ?? String(status)))")
            }
        }
    }

    static func save(_ key: DeviceSigningKey) throws {
        delete()
        let (account, data): (String, Data) = switch key {
        case .secureEnclave(let key): (secureEnclaveAccount, key.dataRepresentation)
        case .software(let key): (softwareAccount, key.rawRepresentation)
        }
        var query = baseQuery(account)
        query[kSecValueData as String] = data
        query[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        let status = SecItemAdd(query as CFDictionary, nil)
        #if targetEnvironment(simulator)
        if status == errSecMissingEntitlement {
            try SimulatorKeyFile.write(data, account: account)
            return
        }
        #endif
        guard status == errSecSuccess else { throw Failure.keychain(status) }
    }

    static func load() throws -> DeviceSigningKey? {
        if let data = read(secureEnclaveAccount) {
            return .secureEnclave(try SecureEnclave.P256.Signing.PrivateKey(dataRepresentation: data))
        }
        if let data = read(softwareAccount) {
            return .software(try P256.Signing.PrivateKey(rawRepresentation: data))
        }
        return nil
    }

    static func delete() {
        for account in [secureEnclaveAccount, softwareAccount] {
            SecItemDelete(baseQuery(account) as CFDictionary)
            #if targetEnvironment(simulator)
            SimulatorKeyFile.remove(account: account)
            #endif
        }
    }

    private static func read(_ account: String) -> Data? {
        var query = baseQuery(account)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        #if targetEnvironment(simulator)
        if status == errSecMissingEntitlement { return SimulatorKeyFile.read(account: account) }
        #endif
        guard status == errSecSuccess else { return nil }
        return result as? Data
    }

    private static func baseQuery(_ account: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
    }
}

#if targetEnvironment(simulator)
/// Bazel's unsigned simulator builds carry no entitlements, so the simulator keychain
/// refuses them (errSecMissingEntitlement). Device builds never take this path.
private enum SimulatorKeyFile {
    private static func url(_ account: String) -> URL {
        URL.applicationSupportDirectory.appending(path: "signing-key-\(account)")
    }

    static func write(_ data: Data, account: String) throws {
        try FileManager.default.createDirectory(at: .applicationSupportDirectory, withIntermediateDirectories: true)
        try data.write(to: url(account), options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
    }

    static func read(account: String) -> Data? {
        try? Data(contentsOf: url(account))
    }

    static func remove(account: String) {
        try? FileManager.default.removeItem(at: url(account))
    }
}
#endif
