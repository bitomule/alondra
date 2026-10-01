import Foundation
import Security

/// App-only persistence, serialized by MainActor. NSE does not access UserDefaults.
@MainActor
public final class PushBindingStore {
    private struct LegacyRecord: Codable {
        let context: String
        let binding: String
    }
    private struct Record: Codable {
        let scope: NotificationBindingScope
        let binding: String
    }
    private struct Collection: Codable {
        let version: Int
        var records: [Record]
        var legacy: LegacyRecord?
    }

    private let defaults: UserDefaults
    private let key: String
    private let randomBytes: () -> Data?

    public init(defaults: UserDefaults = .standard, key: String = "alondra.push-binding.v1", randomBytes: @escaping () -> Data? = PushBindingStore.secureRandomBytes) {
        self.defaults = defaults
        self.key = key
        self.randomBytes = randomBytes
    }

    /// Only call after authenticating the account and approving the device pairing.
    /// Returning to a retained scope reuses its label; another account never adopts it.
    public func binding(scope: NotificationBindingScope) -> NotificationPayload.Binding? {
        guard scope.isValid, var collection = read() else { return nil }
        if let record = collection.records.first(where: { $0.scope == scope }) {
            return NotificationPayload.Binding(record.binding)
        }
        let value: String
        if let legacy = collection.legacy, legacy.context == Self.legacyContext(scope) {
            value = legacy.binding
            collection.legacy = nil
        } else {
            guard let bytes = randomBytes(), bytes.count == 32 else { return nil }
            value = bytes.base64EncodedString()
                .replacingOccurrences(of: "+", with: "-")
                .replacingOccurrences(of: "/", with: "_")
                .replacingOccurrences(of: "=", with: "")
        }
        guard let binding = NotificationPayload.Binding(value),
              !collection.records.contains(where: { $0.binding == value }),
              collection.legacy?.binding != value else { return nil }
        collection.records.append(Record(scope: scope, binding: value))
        guard let data = try? JSONEncoder().encode(collection) else { return nil }
        defaults.set(data, forKey: key)
        return binding
    }

    /// Selective revocation only removes a fully scoped record; unclaimed legacy data
    /// cannot be attributed to an account and remains until migration or explicit logout.
    @discardableResult
    public func remove(scope: NotificationBindingScope) -> Bool {
        guard var collection = read() else { return false }
        collection.records.removeAll { $0.scope == scope }
        guard let data = try? JSONEncoder().encode(collection) else { return false }
        defaults.set(data, forKey: key)
        return true
    }

    /// Explicit logout still removes every retained scope. Selection is not logout.
    public func clear() { defaults.removeObject(forKey: key) }

    private func read() -> Collection? {
        guard let data = defaults.data(forKey: key) else { return Collection(version: 1, records: [], legacy: nil) }
        guard let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
        if object["version"] == nil {
            guard let old = try? JSONDecoder().decode(LegacyRecord.self, from: data),
                  NotificationPayload.Binding(old.binding) != nil else { return nil }
            return Collection(version: 1, records: [], legacy: old)
        }
        guard let collection = try? JSONDecoder().decode(Collection.self, from: data), collection.version == 1,
              Set(collection.records.map(\.scope)).count == collection.records.count,
              Set(collection.records.map(\.binding)).count == collection.records.count,
              collection.records.allSatisfy({ $0.scope.isValid && NotificationPayload.Binding($0.binding) != nil }) else { return nil }
        if let legacy = collection.legacy {
            guard NotificationPayload.Binding(legacy.binding) != nil,
                  !collection.records.contains(where: { $0.binding == legacy.binding }) else { return nil }
        }
        return collection
    }

    private static func legacyContext(_ scope: NotificationBindingScope) -> String {
        let server = scope.relayURL.absoluteString
        return "\(server.count):\(server)\(scope.deviceID.count):\(scope.deviceID)\(scope.credentialRevision.count):\(scope.credentialRevision)"
    }

    nonisolated public static func secureRandomBytes() -> Data? {
        var bytes = [UInt8](repeating: 0, count: 32)
        guard SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes) == errSecSuccess else { return nil }
        return Data(bytes)
    }
}
