import AlondraAccount
import Foundation
import Security
import Testing

struct KeychainRefreshTokenStorageTests {
    private let issuer = URL(string: "https://auth.example.test/realms/alondra")!

    @Test func aTokenSavedForThePreviousIssuerIsDiscardedAndNeverRead() throws {
        let service = "com.bitomule.alondra.tests.\(UUID().uuidString)"
        let storage = KeychainRefreshTokenStorage(service: service, issuer: issuer)
        defer { try? storage.delete() }
        try savePreviousIssuerToken("old.token", service: service)
        #expect(try storage.read() == nil)
        #expect(previousIssuerTokenExists(service: service) == false)
    }

    @Test func aTokenSavedForTheCurrentIssuerSurvivesTheCleanup() throws {
        let service = "com.bitomule.alondra.tests.\(UUID().uuidString)"
        let storage = KeychainRefreshTokenStorage(service: service, issuer: issuer)
        defer { try? storage.delete() }
        try storage.write(RefreshCredential(token: "new.token"))
        #expect(try storage.read()?.token == "new.token")
    }

    private func savePreviousIssuerToken(_ token: String, service: String) throws {
        let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service,
                                    kSecAttrAccount as String: "oidc-refresh-token", kSecValueData as String: Data(token.utf8)]
        #expect(SecItemAdd(query as CFDictionary, nil) == errSecSuccess)
    }

    private func previousIssuerTokenExists(service: String) -> Bool {
        let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service,
                                    kSecAttrAccount as String: "oidc-refresh-token"]
        return SecItemCopyMatching(query as CFDictionary, nil) == errSecSuccess
    }
}
