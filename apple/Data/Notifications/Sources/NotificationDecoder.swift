import AlondraTransport
import CryptoKit
import Foundation
import Security

/// The extension needs only decryption material, never the client's signing key or bearer.
public struct NotificationDecryptionContext: Codable, Equatable, Sendable {
    public let macID: String
    public let deviceID: String
    public let binding: String
    public let macSigningKey: Data
    public let agreementPrivateKey: Data
    public let scope: NotificationBindingScope?

    public init(macID: String, deviceID: String, binding: String,
                macSigningKey: Data, agreementPrivateKey: Data, scope: NotificationBindingScope? = nil) {
        self.macID = macID; self.deviceID = deviceID; self.binding = binding
        self.macSigningKey = macSigningKey; self.agreementPrivateKey = agreementPrivateKey
        self.scope = scope
    }
}

public enum NotificationDecryptionError: Error, Equatable, Sendable {
    case keychain(OSStatus), invalidPayload, invalidCollection, unsupportedVersion, identityConflict
}

public enum NotificationDecoder {
    /// An untrusted lookup hint only. No plaintext or action is accepted until decrypt verifies it.
    public static func bindingHint(_ outerJSON: Data) throws -> NotificationPayload.Binding {
        let outer = try JSONDecoder().decode(Outer.self, from: outerJSON)
        guard let binding = NotificationPayload.Binding(outer.alondra.pushBinding) else {
            throw NotificationDecryptionError.invalidPayload
        }
        return binding
    }

    /// Never trust routing or buttons supplied outside the authenticated encrypted payload.
    /// Notifications do not expire: an action is valid whenever it is tapped and the server
    /// refuses it if the item changed since.
    public static func decrypt(_ outerJSON: Data, context: NotificationDecryptionContext) throws -> Data {
        let outer = try JSONDecoder().decode(Outer.self, from: outerJSON)
        if let scope = context.scope {
            guard scope.isValid, scope.macID == context.macID, scope.deviceID == context.deviceID else {
                throw NotificationDecryptionError.invalidPayload
            }
        }
        guard outer.alondra.pushBinding == context.binding,
              NotificationPayload.Binding(context.binding) != nil,
              outer.alondra.envelope.kind == "push" else { throw NotificationDecryptionError.invalidPayload }
        let plaintext = try outer.alondra.envelope.open(
            senderKey: P256.Signing.PublicKey(rawRepresentation: context.macSigningKey),
            recipientKey: P256.KeyAgreement.PrivateKey(rawRepresentation: context.agreementPrivateKey),
            sender: context.macID, recipient: context.deviceID, route: context.macID, nowMilliseconds: nil)
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
