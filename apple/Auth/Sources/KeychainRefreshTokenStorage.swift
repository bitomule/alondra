import Foundation
import Security

public struct KeychainRefreshTokenStorage: RefreshTokenStorage {
    private let service: String
    private let account = "oidc-refresh-token"

    public init(service: String = "com.bitomule.alondra.auth") { self.service = service }

    public func read() throws -> RefreshCredential? {
        var query = baseQuery
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess else { throw AuthError.keychain(status) }
        guard let data = result as? Data else {
            throw AuthError.invalidToken
        }
        return try RefreshCredential.decodePersisted(data)
    }

    public func write(_ credential: RefreshCredential) throws {
        let data = try credential.persistedData()
        let status = SecItemUpdate(baseQuery as CFDictionary, [kSecValueData as String: data] as CFDictionary)
        if status == errSecSuccess { return }
        guard status == errSecItemNotFound else { throw AuthError.keychain(status) }
        var query = baseQuery
        query[kSecValueData as String] = data
        query[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        let add = SecItemAdd(query as CFDictionary, nil)
        guard add == errSecSuccess else { throw AuthError.keychain(add) }
    }

    public func delete() throws {
        let status = SecItemDelete(baseQuery as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else { throw AuthError.keychain(status) }
    }

    private var baseQuery: [String: Any] {
        [kSecClass as String: kSecClassGenericPassword,
         kSecAttrService as String: service,
         kSecAttrAccount as String: account]
    }
}
