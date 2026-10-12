import AlondraContract
import CryptoKit
import Darwin
import Foundation

/// Only the old reader remains: importing a legacy intent never gives it dispatch permission.
final class LegacyIntentFiles: Sendable {
    private static let keyDomain = Data("alondra-action-intents-v1".utf8)
    private let directory: URL
    let file: URL
    private let marker: URL
    private let lock: URL
    private let key: SymmetricKey
    private let binding: Data
    private let completedLimit: Int

    init(directory: URL, keyMaterial: Data, scope: ActionIntentScope, completedLimit: Int) throws {
        guard directory.isFileURL, completedLimit > 0, keyMaterial.count == 32, !scope.service.isEmpty,
              !scope.device.isEmpty, !scope.credentialRevision.isEmpty else { throw ViewActionIntentError.invalidScope }
        let encoder = JSONEncoder(); encoder.outputFormatting = [.sortedKeys]
        let binding = try encoder.encode(scope)
        let digest = SHA256.hash(data: binding).map { String(format: "%02x", $0) }.joined()
        self.directory = directory; self.binding = binding; self.completedLimit = completedLimit
        file = directory.appending(path: digest + ".intents")
        marker = directory.appending(path: digest + ".initialized")
        lock = directory.appending(path: digest + ".lock")
        key = HKDF<SHA256>.deriveKey(inputKeyMaterial: SymmetricKey(data: keyMaterial),
            salt: binding, info: Self.keyDomain, outputByteCount: 32)
    }
    func locked<T>(_ work: () throws -> T) throws -> T {
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true,
                                                    attributes: [.posixPermissions: 0o700])
            let directoryFD = open(directory.path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
            guard directoryFD >= 0 else { throw ViewActionIntentError.unreadableStore }
            defer { close(directoryFD) }
            let descriptor = open(lock.path, O_CREAT | O_RDWR | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK, 0o600)
            guard descriptor >= 0 else { throw ViewActionIntentError.unreadableStore }
            defer { close(descriptor) }
            var lockInfo = stat()
            guard fstat(descriptor, &lockInfo) == 0, lockInfo.st_mode & S_IFMT == S_IFREG,
                  flock(descriptor, LOCK_EX) == 0 else { throw ViewActionIntentError.unreadableStore }
            defer { _ = flock(descriptor, LOCK_UN) }
            return try work()
        } catch let error as ViewActionIntentError { throw error }
        catch { throw ViewActionIntentError.unreadableStore }
    }
    func existingSnapshot() throws -> ViewActionIntentStore.Snapshot? {
        let metadata = try read(marker, limit: 32)
        let ciphertext = try read(file, limit: ViewActionIntentStore.maxBytes + 64)
        if metadata == nil && ciphertext == nil { return nil }
        guard metadata == Data(SHA256.hash(data: binding)), let ciphertext else { throw ViewActionIntentError.unreadableStore }
        do {
            let plaintext = try AES.GCM.open(AES.GCM.SealedBox(combined: ciphertext), using: key, authenticating: binding)
            let snapshot = try JSONDecoder().decode(ViewActionIntentStore.Snapshot.self, from: plaintext)
            guard snapshot.pending.count <= ViewActionIntentStore.maxPending,
                  snapshot.pending.count + snapshot.completed.count <= completedLimit,
                  Set(snapshot.pending.map { $0.request.commandId } + snapshot.completed.map(\.commandID)).count
                    == snapshot.pending.count + snapshot.completed.count else { throw ViewActionIntentError.unreadableStore }
            for record in snapshot.pending { try ViewActionIntentStore.validate(record) }
            guard snapshot.completed.allSatisfy({ !$0.commandID.isEmpty && !$0.dataEpoch.isEmpty && $0.bodyDigest.count == 32 })
            else { throw ViewActionIntentError.unreadableStore }
            return snapshot
        } catch { throw ViewActionIntentError.unreadableStore }
    }
    func read(_ url: URL, limit: Int) throws -> Data? {
        let descriptor = open(url.path, O_RDONLY | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK)
        if descriptor < 0 {
            if errno == ENOENT { return nil }
            throw ViewActionIntentError.unreadableStore
        }
        defer { close(descriptor) }
        var metadata = stat()
        guard fstat(descriptor, &metadata) == 0, metadata.st_mode & S_IFMT == S_IFREG,
              metadata.st_size >= 0, metadata.st_size <= limit else { throw ViewActionIntentError.unreadableStore }
        var bytes = Data(count: Int(metadata.st_size))
        let complete = bytes.withUnsafeMutableBytes { buffer -> Bool in
            var offset = 0
            while offset < buffer.count {
                let count = Darwin.read(descriptor, buffer.baseAddress!.advanced(by: offset), buffer.count - offset)
                if count < 0 && errno == EINTR { continue }
                guard count > 0 else { return false }
                offset += count
            }
            return true
        }
        guard complete else { throw ViewActionIntentError.unreadableStore }
        return bytes
    }
}
