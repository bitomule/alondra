import AlondraNotificationPayload
import Foundation
import Security

/// App-owned persistence. The extension only reads the label in the APNs payload.
@MainActor
public final class PushBindingStore {
    private struct Record: Codable {
        let context: String
        let binding: String
    }

    private let defaults: UserDefaults
    private let key: String
    private let randomBytes: () -> Data?

    public init(defaults: UserDefaults = .standard, key: String = "alondra.push-binding.v1", randomBytes: @escaping () -> Data? = PushBindingStore.secureRandomBytes) {
        self.defaults = defaults
        self.key = key
        self.randomBytes = randomBytes
    }

    /// A changed URL, device ID, or credential always gets a fresh binding.
    public func binding(server: URL, deviceID: String, credentialRevision: String) -> NotificationPayload.Binding? {
        let context = "\(server.absoluteString.count):\(server.absoluteString)\(deviceID.count):\(deviceID)\(credentialRevision.count):\(credentialRevision)"
        if let data = defaults.data(forKey: key),
           let record = try? JSONDecoder().decode(Record.self, from: data),
           record.context == context,
           let binding = NotificationPayload.Binding(record.binding) { return binding }

        guard let bytes = randomBytes(), bytes.count == 32 else { return nil }
        let value = bytes.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
        guard let binding = NotificationPayload.Binding(value),
              let data = try? JSONEncoder().encode(Record(context: context, binding: value)) else { return nil }
        defaults.set(data, forKey: key)
        return binding
    }

    public func clear() { defaults.removeObject(forKey: key) }

    public static func secureRandomBytes() -> Data? {
        var bytes = [UInt8](repeating: 0, count: 32)
        guard SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes) == errSecSuccess else { return nil }
        return Data(bytes)
    }
}
