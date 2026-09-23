import CryptoKit
import Foundation
import Security

/// Keeps this device's signing key.
///
/// A Secure Enclave key's `dataRepresentation` is wrapped by this device's enclave and useless
/// anywhere else. On macOS it lives in a file: the Mac app is ad-hoc signed, so the data
/// protection keychain refuses it (errSecMissingEntitlement) and the login keychain would ask
/// for a password after every rebuild. Software keys always go to the keychain.
public enum DeviceKeyStore {
    private static let service = (Bundle.main.bundleIdentifier ?? "alondra") + ".signing-key"
    private static let secureEnclaveAccount = "secure-enclave"
    private static let softwareAccount = "software"

    public enum Failure: LocalizedError {
        case keychain(OSStatus)

        public var errorDescription: String? {
            switch self {
            case .keychain(let status):
                String(localized: "Couldn’t save the key to the keychain (\(SecCopyErrorMessageString(status, nil) as String? ?? String(status)))", table: "AlondraShared", bundle: #bundle)
            }
        }
    }

    public static func save(_ key: DeviceSigningKey) throws {
        delete()
        switch key {
        case .secureEnclave(let key):
            #if os(macOS)
            try KeyFile.write(key.dataRepresentation, account: secureEnclaveAccount)
            #else
            try saveToKeychain(key.dataRepresentation, account: secureEnclaveAccount)
            #endif
        case .software(let key):
            try saveToKeychain(key.rawRepresentation, account: softwareAccount)
        }
    }

    public static func load() throws -> DeviceSigningKey? {
        #if os(macOS)
        let enclaveData = KeyFile.read(account: secureEnclaveAccount)
        #else
        let enclaveData = readFromKeychain(secureEnclaveAccount)
        #endif
        if let enclaveData {
            return .secureEnclave(try SecureEnclave.P256.Signing.PrivateKey(dataRepresentation: enclaveData))
        }
        if let data = readFromKeychain(softwareAccount) {
            return .software(try P256.Signing.PrivateKey(rawRepresentation: data))
        }
        return nil
    }

    public static func delete() {
        for account in [secureEnclaveAccount, softwareAccount] {
            SecItemDelete(baseQuery(account) as CFDictionary)
            KeyFile.remove(account: account)
        }
    }

    private static func saveToKeychain(_ data: Data, account: String) throws {
        var query = baseQuery(account)
        query[kSecValueData as String] = data
        query[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        let status = SecItemAdd(query as CFDictionary, nil)
        #if targetEnvironment(simulator)
        if status == errSecMissingEntitlement {
            try KeyFile.write(data, account: account)
            return
        }
        #endif
        guard status == errSecSuccess else { throw Failure.keychain(status) }
    }

    private static func readFromKeychain(_ account: String) -> Data? {
        var query = baseQuery(account)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        #if targetEnvironment(simulator)
        if status == errSecMissingEntitlement { return KeyFile.read(account: account) }
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

/// Also used by Bazel's unsigned simulator builds, which carry no entitlements, so the
/// simulator keychain refuses them. Device builds of the iOS app never write here.
private enum KeyFile {
    private static var directory: URL {
        #if os(macOS)
        URL.applicationSupportDirectory.appending(path: Bundle.main.bundleIdentifier ?? "alondra")
        #else
        URL.applicationSupportDirectory
        #endif
    }

    private static func url(_ account: String) -> URL {
        directory.appending(path: "signing-key-\(account)")
    }

    static func write(_ data: Data, account: String) throws {
        #if os(macOS)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        try data.write(to: url(account), options: .atomic)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url(account).path(percentEncoded: false))
        #else
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try data.write(to: url(account), options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
        #endif
    }

    static func read(account: String) -> Data? {
        try? Data(contentsOf: url(account))
    }

    static func remove(account: String) {
        try? FileManager.default.removeItem(at: url(account))
    }
}
