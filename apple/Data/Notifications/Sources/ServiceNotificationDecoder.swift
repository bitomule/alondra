import AlondraNotificationPayload
import AlondraRelayClient
import CryptoKit
import Foundation
import Security

/// The extension needs only decryption material, never the client's signing key or bearer.
public struct NotificationDecryptionContext: Codable, Sendable {
    public let macID: String
    public let deviceID: String
    public let binding: String
    public let macSigningKey: Data
    public let agreementPrivateKey: Data

    public init(macID: String, deviceID: String, binding: String,
                macSigningKey: Data, agreementPrivateKey: Data) {
        self.macID = macID; self.deviceID = deviceID; self.binding = binding
        self.macSigningKey = macSigningKey; self.agreementPrivateKey = agreementPrivateKey
    }
}

public struct NotificationDecryptionStore: Sendable {
    private let accessGroup: String
    public init(accessGroup: String) { self.accessGroup = accessGroup }
    private var query: [String: Any] {
        [kSecClass as String: kSecClassGenericPassword,
         kSecAttrService as String: "com.bitomule.alondra.notification-decryption.v1",
         kSecAttrAccount as String: "approved-client", kSecAttrAccessGroup as String: accessGroup]
    }
    public func load() throws -> NotificationDecryptionContext? {
        var q = query
        q[kSecReturnData as String] = true
        q[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(q as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = result as? Data else { throw NotificationDecryptionError.keychain(status) }
        return try JSONDecoder().decode(NotificationDecryptionContext.self, from: data)
    }
    public func save(_ context: NotificationDecryptionContext) throws {
        let data = try JSONEncoder().encode(context)
        var q = query
        q[kSecValueData as String] = data
        q[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        let status = SecItemAdd(q as CFDictionary, nil)
        if status == errSecDuplicateItem {
            let updated = SecItemUpdate(query as CFDictionary, [kSecValueData as String: data] as CFDictionary)
            guard updated == errSecSuccess else { throw NotificationDecryptionError.keychain(updated) }
        } else if status != errSecSuccess { throw NotificationDecryptionError.keychain(status) }
    }
    public func clear() throws {
        let status = SecItemDelete(query as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else { throw NotificationDecryptionError.keychain(status) }
    }
}

public enum NotificationDecryptionError: Error { case keychain(OSStatus), invalidPayload }

public enum ServiceNotificationDecoder {
    /// Never trust routing or buttons supplied outside the authenticated encrypted payload.
    public static func decrypt(_ outerJSON: Data, context: NotificationDecryptionContext,
                               nowMilliseconds: UInt64) throws -> Data {
        let outer = try JSONDecoder().decode(Outer.self, from: outerJSON)
        guard outer.alondra.pushBinding == context.binding,
              NotificationPayload.Binding(context.binding) != nil,
              outer.alondra.envelope.kind == "push" else { throw NotificationDecryptionError.invalidPayload }
        let plaintext = try outer.alondra.envelope.open(
            senderKey: P256.Signing.PublicKey(rawRepresentation: context.macSigningKey),
            recipientKey: P256.KeyAgreement.PrivateKey(rawRepresentation: context.agreementPrivateKey),
            sender: context.macID, recipient: context.deviceID, route: context.macID, nowMilliseconds: nowMilliseconds)
        guard let decoded = try JSONSerialization.jsonObject(with: plaintext) as? [String: Any],
              decoded["push_binding"] as? String == context.binding,
              var payload = decoded["payload"] as? [String: Any],
              var item = payload["alondra"] as? [String: Any],
              var aps = payload["aps"] as? [String: Any] else { throw NotificationDecryptionError.invalidPayload }
        item["push_binding"] = context.binding
        payload["alondra"] = item
        if decoded["with_sound"] as? Bool != true { aps.removeValue(forKey: "sound") }
        payload["aps"] = aps
        return try JSONSerialization.data(withJSONObject: payload)
    }
    private struct Outer: Decodable {
        let alondra: Encrypted
        struct Encrypted: Decodable {
            let envelope: EnvelopeV1
            let pushBinding: String
            enum CodingKeys: String, CodingKey { case envelope, pushBinding = "push_binding" }
        }
    }
}
