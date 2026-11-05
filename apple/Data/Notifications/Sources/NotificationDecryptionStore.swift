import Foundation
import Security
import Synchronization

struct NotificationDecryptionCollection: Codable, Sendable {
    private(set) var contexts: [NotificationDecryptionContext] = []
    init() {}
    private enum CodingKeys: String, CodingKey { case version, contexts }

    init(from decoder: any Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        if !values.contains(.version) {
            contexts = [try NotificationDecryptionContext(from: decoder)]
        } else {
            guard try values.decode(Int.self, forKey: .version) == 1 else {
                throw NotificationDecryptionError.unsupportedVersion
            }
            contexts = try values.decode([NotificationDecryptionContext].self, forKey: .contexts)
        }
        guard Set(contexts.map(\.binding)).count == contexts.count,
              contexts.allSatisfy(Self.valid) else { throw NotificationDecryptionError.invalidCollection }
    }

    func encode(to encoder: any Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        try values.encode(1, forKey: .version)
        try values.encode(contexts, forKey: .contexts)
    }

    func context(for binding: NotificationPayload.Binding) -> NotificationDecryptionContext? {
        contexts.first { $0.binding == binding.value }
    }

    mutating func upsert(_ context: NotificationDecryptionContext) throws {
        guard Self.valid(context), context.scope != nil else { throw NotificationDecryptionError.invalidCollection }
        if let index = contexts.firstIndex(where: { $0.binding == context.binding }) {
            let old = contexts[index]
            guard old.macID == context.macID, old.deviceID == context.deviceID,
                  old.macSigningKey == context.macSigningKey,
                  old.agreementPrivateKey == context.agreementPrivateKey,
                  old.scope == nil || old.scope == context.scope else {
                throw NotificationDecryptionError.identityConflict
            }
            // Legacy scope is adopted only after the app supplies its verified pairing;
            // all original keys, IDs and the opaque binding must match exactly.
            contexts[index] = context
        } else {
            contexts.append(context)
        }
    }

    mutating func remove(_ binding: NotificationPayload.Binding) {
        contexts.removeAll { $0.binding == binding.value }
    }

    private static func valid(_ context: NotificationDecryptionContext) -> Bool {
        guard NotificationPayload.Binding(context.binding) != nil,
              !context.macID.isEmpty, !context.deviceID.isEmpty else { return false }
        guard let scope = context.scope else { return true } // retained legacy bytes
        return scope.isValid && scope.macID == context.macID && scope.deviceID == context.deviceID
    }
}

/// App is the sole writer; NSE reads atomic snapshots and selects by binding.
/// The item contains agreement private keys and Mac pins, never account bearers or signing keys.
public struct NotificationDecryptionStore: Sendable {
    private static let transaction = Mutex(())
    private let item: any NotificationDecryptionItemStorage

    public init(accessGroup: String) { item = KeychainNotificationDecryptionItem(accessGroup: accessGroup) }
    init(item: any NotificationDecryptionItemStorage) { self.item = item }

    public func load(binding: NotificationPayload.Binding) throws -> NotificationDecryptionContext? {
        try Self.transaction.withLock { _ in try read().context(for: binding) }
    }

    /// Every pairing's keys, for a reader that goes by the Mac a message came from.
    public func contexts() throws -> [NotificationDecryptionContext] {
        try Self.transaction.withLock { _ in try read().contexts }
    }

    public func save(_ context: NotificationDecryptionContext) throws {
        try Self.transaction.withLock { _ in
            var collection = try read()
            try collection.upsert(context)
            try item.write(JSONEncoder().encode(collection))
        }
    }

    /// Publish exactly the currently authenticated account's approved contexts in one
    /// Keychain update. Validate collisions against existing bytes before pruning.
    public func replaceAuthorizedContexts(_ contexts: [NotificationDecryptionContext]) throws {
        try Self.transaction.withLock { _ in
            var existing = try read()
            var replacement = NotificationDecryptionCollection()
            for context in contexts {
                try existing.upsert(context)
                try replacement.upsert(context)
            }
            guard Set(contexts.map(\.binding)).count == contexts.count else {
                throw NotificationDecryptionError.invalidCollection
            }
            try item.write(JSONEncoder().encode(replacement))
        }
    }

    public func remove(binding: NotificationPayload.Binding) throws {
        try Self.transaction.withLock { _ in
            var collection = try read()
            collection.remove(binding)
            try item.write(JSONEncoder().encode(collection))
        }
    }

    /// Logout remains explicit full cleanup; callers must not use it merely to select a Mac.
    public func clear() throws {
        try Self.transaction.withLock { _ in try item.clear() }
    }

    private func read() throws -> NotificationDecryptionCollection {
        guard let bytes = try item.read() else { return NotificationDecryptionCollection() }
        return try JSONDecoder().decode(NotificationDecryptionCollection.self, from: bytes)
    }
}

/// The id this phone's pairings share at the relay, so it knows they feed one Live Activity. It is
/// made once and stays in the Keychain, beside the keys it travels with.
public struct ActivityPhoneIdentity: Sendable {
    private let item: any NotificationDecryptionItemStorage

    public init(accessGroup: String) { item = KeychainNotificationDecryptionItem(accessGroup: accessGroup, account: "live-activity-phone") }
    init(item: any NotificationDecryptionItemStorage) { self.item = item }

    public func id() throws -> String {
        if let saved = try item.read() { return String(decoding: saved, as: UTF8.self) }
        let made = UUID().uuidString
        try item.write(Data(made.utf8))
        return made
    }

    public func forget() throws { try item.clear() }
}

protocol NotificationDecryptionItemStorage: Sendable {
    func read() throws -> Data?
    func write(_ data: Data) throws
    func clear() throws
}

struct KeychainNotificationDecryptionItem: NotificationDecryptionItemStorage {
    let accessGroup: String
    var account = "approved-client"
    private var query: [String: Any] {
        [kSecClass as String: kSecClassGenericPassword,
         kSecAttrService as String: "com.bitomule.alondra.notification-decryption.v1",
         kSecAttrAccount as String: account, kSecAttrAccessGroup as String: accessGroup]
    }
    func read() throws -> Data? {
        var q = query
        q[kSecReturnData as String] = true
        q[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(q as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = result as? Data else { throw NotificationDecryptionError.keychain(status) }
        return data
    }
    func write(_ data: Data) throws {
        let updated = SecItemUpdate(query as CFDictionary, [kSecValueData as String: data] as CFDictionary)
        if updated == errSecSuccess { return }
        guard updated == errSecItemNotFound else { throw NotificationDecryptionError.keychain(updated) }
        var q = query
        q[kSecValueData as String] = data
        q[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        let status = SecItemAdd(q as CFDictionary, nil)
        guard status == errSecSuccess else { throw NotificationDecryptionError.keychain(status) }
    }
    func clear() throws {
        let status = SecItemDelete(query as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else { throw NotificationDecryptionError.keychain(status) }
    }
}
