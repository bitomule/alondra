import Foundation
import Security

public protocol ApprovedPairingStore: Sendable {
    func load() throws -> ApprovedServicePairing?
    func save(_ pairing: ApprovedServicePairing) throws
    func delete() throws
}

/// One Keychain item keeps the approved identity and both private keys atomic. It never syncs to iCloud.
public struct KeychainApprovedPairingStore: ApprovedPairingStore {
    private let service: String

    public init(service: String = "com.bitomule.alondra.service-pairing.v1") {
        self.service = service
    }

    private var key: [String: Any] {
        [kSecClass as String: kSecClassGenericPassword,
         kSecAttrService as String: service,
         kSecAttrAccount as String: "approved-phone"]
    }

    public func load() throws -> ApprovedServicePairing? {
        var query = key
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = result as? Data else { throw PairingStoreError.keychain(status) }
        return try JSONDecoder().decode(ApprovedServicePairing.self, from: data)
    }

    public func save(_ pairing: ApprovedServicePairing) throws {
        let data = try JSONEncoder().encode(pairing)
        var query = key
        query[kSecValueData as String] = data
        query[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
        let status = SecItemAdd(query as CFDictionary, nil)
        if status == errSecDuplicateItem {
            let updated = SecItemUpdate(key as CFDictionary,
                [kSecValueData as String: data] as CFDictionary)
            guard updated == errSecSuccess else { throw PairingStoreError.keychain(updated) }
        } else if status != errSecSuccess {
            throw PairingStoreError.keychain(status)
        }
    }

    public func delete() throws {
        let status = SecItemDelete(key as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw PairingStoreError.keychain(status)
        }
    }
}

public enum PairingStoreError: Error, Sendable {
    case keychain(OSStatus)
}
