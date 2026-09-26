import CryptoKit
import Foundation
import Security

public protocol DeviceKeyStorage {
    func save(_ key: DeviceSigningKey) throws
    func load() throws -> DeviceSigningKey?
    func delete()
}

/// Uses the same service, accounts and file paths as the original app storage.
/// Secure Enclave keys are wrapped by the device; software keys stay in Keychain.
public struct SystemDeviceKeyStorage: DeviceKeyStorage {
    public init() {}

    public func save(_ key: DeviceSigningKey) throws { try DeviceKeyStore.save(key) }
    public func load() throws -> DeviceSigningKey? { try DeviceKeyStore.load() }
    public func delete() { DeviceKeyStore.delete() }
}

public enum DeviceKeyStore {
    private static let service = (Bundle.main.bundleIdentifier ?? "alondra") + ".signing-key"
    private static let secureEnclaveAccount = "secure-enclave"
    private static let softwareAccount = "software"

    public enum Failure: LocalizedError {
        case keychain(OSStatus)
        case invalidStoredKey

        public var errorDescription: String? {
            switch self {
            case .keychain(let status):
                "Couldn’t save the key to the keychain (\(SecCopyErrorMessageString(status, nil) as String? ?? String(status)))"
            case .invalidStoredKey:
                "The saved device signing key is no longer valid. Pair this device again."
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
        let enclaveData = try KeyFile.read(account: secureEnclaveAccount)
        #else
        let enclaveData = try readFromKeychain(secureEnclaveAccount)
        #endif
        if let enclaveData {
            do { return .secureEnclave(try SecureEnclave.P256.Signing.PrivateKey(dataRepresentation: enclaveData)) }
            catch { throw Failure.invalidStoredKey }
        }
        if let data = try readFromKeychain(softwareAccount) {
            do { return .software(try P256.Signing.PrivateKey(rawRepresentation: data)) }
            catch { throw Failure.invalidStoredKey }
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

    private static func readFromKeychain(_ account: String) throws -> Data? {
        var query = baseQuery(account)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        #if targetEnvironment(simulator)
        if status == errSecMissingEntitlement { return try KeyFile.read(account: account) }
        #endif
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess else { throw Failure.keychain(status) }
        guard let data = result as? Data else { throw Failure.invalidStoredKey }
        return data
    }

    private static func baseQuery(_ account: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
    }
}

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

    static func read(account: String) throws -> Data? {
        do { return try Data(contentsOf: url(account)) }
        catch let error as CocoaError where error.code == .fileReadNoSuchFile { return nil }
    }

    static func remove(account: String) {
        try? FileManager.default.removeItem(at: url(account))
    }
}
