import Foundation
import Security
import Synchronization

public protocol ApprovedPairingStore: Sendable {
    func loadCollection() throws -> ApprovedPairingCollection
    func select(_ id: ApprovedPairingID) throws
    func restoreSelection(_ previous: ApprovedPairingID, ifSelected attempted: ApprovedPairingID) throws
    func load() throws -> ApprovedServicePairing?
    func save(_ pairing: ApprovedServicePairing) throws
    /// Explicit logout removes every locally retained pairing, not only selection.
    func delete() throws
    /// One read-modify-write of the whole collection; nothing is written when `change` throws.
    func modify(_ change: (inout ApprovedPairingCollection) throws -> Void) throws
}

/// Synchronous storage avoids adding a suspension between approval and persistence.
/// This item is owned by the app process; the notification extension does not write it.
public struct KeychainApprovedPairingStore: ApprovedPairingStore {
    // Shared by all instances, including independently constructed clients. SecItemUpdate
    // is atomic, but read/modify/write needs this process-local critical section too.
    // This is not a cross-process lock; new writers must use a coordinated design.
    private static let transaction = Mutex(())
    private let item: any PairingItemStorage

    public init(service: String = "com.bitomule.alondra.service-pairing.v1") {
        item = KeychainPairingItem(service: service)
    }

    init(item: any PairingItemStorage) { self.item = item }

    public func loadCollection() throws -> ApprovedPairingCollection {
        try Self.transaction.withLock { _ in try readCollection() }
    }

    public func load() throws -> ApprovedServicePairing? {
        try loadCollection().selected
    }

    public func save(_ pairing: ApprovedServicePairing) throws {
        try Self.transaction.withLock { _ in
            var collection = try readCollection()
            try collection.upsert(pairing)
            try item.write(JSONEncoder().encode(collection))
        }
    }

    public func select(_ id: ApprovedPairingID) throws {
        try Self.transaction.withLock { _ in
            var collection = try readCollection()
            try collection.select(id)
            try item.write(JSONEncoder().encode(collection))
        }
    }

    /// Compensate a failed publication only while its selection is still current.
    /// Never reconstruct a deleted collection or overwrite a newer approval.
    public func restoreSelection(_ previous: ApprovedPairingID, ifSelected attempted: ApprovedPairingID) throws {
        try Self.transaction.withLock { _ in
            var collection = try readCollection()
            guard collection.selectedID == attempted,
                  collection.pairings.contains(where: { ApprovedPairingID($0) == previous }) else { return }
            try collection.select(previous)
            try item.write(JSONEncoder().encode(collection))
        }
    }

    public func delete() throws {
        try Self.transaction.withLock { _ in try item.delete() }
    }

    public func modify(_ change: (inout ApprovedPairingCollection) throws -> Void) throws {
        try Self.transaction.withLock { _ in
            var collection = try readCollection()
            try change(&collection)
            try item.write(JSONEncoder().encode(collection))
        }
    }

    private func readCollection() throws -> ApprovedPairingCollection {
        guard let data = try item.read() else { return ApprovedPairingCollection() }
        return try JSONDecoder().decode(ApprovedPairingCollection.self, from: data)
    }
}

/// Narrow I/O seam lets migration/failure tests exercise the real transaction without
/// accessing a user's Keychain. Only item-not-found is represented by nil.
protocol PairingItemStorage: Sendable {
    func read() throws -> Data?
    func write(_ data: Data) throws
    func delete() throws
}

private struct KeychainPairingItem: PairingItemStorage {
    let service: String

    private var key: [String: Any] {
        [kSecClass as String: kSecClassGenericPassword,
         kSecAttrService as String: service,
         kSecAttrAccount as String: "approved-phone"]
    }

    func read() throws -> Data? {
        var query = key
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = result as? Data else { throw PairingStoreError.keychain(status) }
        return data
    }

    func write(_ data: Data) throws {
        // Update existing bytes in place; never delete-and-add or dual-write legacy data.
        let updated = SecItemUpdate(key as CFDictionary,
            [kSecValueData as String: data] as CFDictionary)
        if updated == errSecSuccess { return }
        guard updated == errSecItemNotFound else { throw PairingStoreError.keychain(updated) }
        var query = key
        query[kSecValueData as String] = data
        query[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
        let status = SecItemAdd(query as CFDictionary, nil)
        guard status == errSecSuccess else { throw PairingStoreError.keychain(status) }
    }

    func delete() throws {
        let status = SecItemDelete(key as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw PairingStoreError.keychain(status)
        }
    }
}

public enum PairingStoreError: Error, Equatable, Sendable {
    case keychain(OSStatus)
    case unsupportedVersion
    case invalidCollection
    case identityConflict
}
