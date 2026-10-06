import Foundation
import Synchronization

/// Bindings whose pushes arrived after this iPhone's keys were gone. The notification
/// extension shares no app group with the app, only the push Keychain group.
public struct OrphanBindingStore: Sendable {
    public static let limit = 32
    private static let transaction = Mutex(())
    private let item: any NotificationDecryptionItemStorage

    public init(accessGroup: String) {
        item = KeychainNotificationDecryptionItem(accessGroup: accessGroup, account: "orphan-bindings")
    }
    init(item: any NotificationDecryptionItemStorage) { self.item = item }

    public func append(_ binding: NotificationPayload.Binding) throws {
        try Self.transaction.withLock { _ in
            var values = try read()
            values.removeAll { $0 == binding.value }
            values.append(binding.value)
            try item.write(JSONEncoder().encode(Array(values.suffix(Self.limit))))
        }
    }

    public func load() throws -> [NotificationPayload.Binding] {
        try Self.transaction.withLock { _ in try read().compactMap(NotificationPayload.Binding.init) }
    }

    public func remove(_ bindings: [NotificationPayload.Binding]) throws {
        guard !bindings.isEmpty else { return }
        let removed = Set(bindings.map(\.value))
        try Self.transaction.withLock { _ in
            let values = try read()
            let kept = values.filter { !removed.contains($0) }
            guard kept.count != values.count else { return }
            if kept.isEmpty { try item.clear() } else { try item.write(JSONEncoder().encode(kept)) }
        }
    }

    private func read() throws -> [String] {
        guard let bytes = try item.read() else { return [] }
        return (try? JSONDecoder().decode([String].self, from: bytes)) ?? []
    }
}
