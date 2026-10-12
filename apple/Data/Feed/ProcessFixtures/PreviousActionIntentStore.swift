// Test-only baseline copied from 75effee0; only type names were changed.
import AlondraContract
import CryptoKit
import Darwin
import Foundation

public enum PreviousActionIntentError: Error, Equatable, Sendable {
    case invalidScope, invalidRecord, unreadableStore, capacityExceeded
}

public struct PreviousActionIntentScope: Codable, Equatable, Sendable {
    public let service: String
    public let device: String
    public let credentialRevision: String
    public init(service: String, device: String, credentialRevision: String) {
        self.service = service; self.device = device; self.credentialRevision = credentialRevision
    }
}

/// Stores intent durably before dispatch, and verified outcomes before presentation.
public actor PreviousActionIntentStore {
    public struct Record: Codable, Sendable {
        let request: ActionRequest
        let body: Data
        let resolvesItsCard: Bool
        init(request: ActionRequest, body: Data) {
            self.request = request; self.body = body; resolvesItsCard = request.resolvesItsCard
        }
        var restoredRequest: ActionRequest {
            var value = request
            value.resolvesItsCard = resolvesItsCard
            return value
        }
    }
    struct Completed: Codable, Sendable {
        let commandID: String
        let bodyDigest: Data
        let dataEpoch: String
        var response: ActionResponse?
    }
    enum Reservation: Sendable {
        case created(Record), pending(Record), completed(ActionResponse?)
    }
    struct Snapshot: Codable, Sendable {
        var pending: [Record] = []
        var completed: [Completed] = []
    }
    static let maxPending = 128
    static let maxCompleted = 16_384
    static let maxBytes = 4 * 1024 * 1024
    public nonisolated let restored: [Record]
    private let files: PreviousIntentFiles

    public init(directory: URL, keyMaterial: Data, scope: PreviousActionIntentScope) async throws {
        try await self.init(directory: directory, keyMaterial: keyMaterial, scope: scope, completedLimit: Self.maxCompleted)
    }

    init(directory: URL, keyMaterial: Data, scope: PreviousActionIntentScope, completedLimit: Int) async throws {
        let files = try PreviousIntentFiles(directory: directory, keyMaterial: keyMaterial, scope: scope, completedLimit: completedLimit)
        restored = try await files.run { try files.locked { try files.load(initialize: true).pending } }
        self.files = files
    }

    func reserve(_ request: ActionRequest, body: Data) async throws -> Reservation {
        let record = Record(request: request, body: body)
        try Self.validate(record)
        let files = files
        return try await files.run { try files.locked {
            var snapshot = try files.load()
            if let completed = snapshot.completed.first(where: { $0.commandID == request.commandId }) {
                guard completed.bodyDigest == Data(SHA256.hash(data: body)), completed.dataEpoch == request.dataEpoch
                else { throw PreviousActionIntentError.invalidRecord }
                return .completed(completed.response)
            }
            if let existing = snapshot.pending.first(where: { $0.request.commandId == request.commandId }) {
                guard existing.body == body else { throw PreviousActionIntentError.invalidRecord }
                return .pending(existing)
            }
            if let existing = snapshot.pending.first(where: { Self.sameIntent($0.request, request) }) { return .pending(existing) }
            guard snapshot.pending.count < Self.maxPending, snapshot.pending.count + snapshot.completed.count < files.completedLimit
            else { throw PreviousActionIntentError.capacityExceeded }
            snapshot.pending.append(record)
            try files.save(snapshot)
            return .created(record)
        } }
    }

    func complete(_ record: Record, response: ActionResponse) async throws {
        let files = files
        try await files.run { try files.locked {
            var snapshot = try files.load()
            if snapshot.completed.contains(where: { $0.commandID == record.request.commandId }) { return }
            guard let index = snapshot.pending.firstIndex(where: { $0.request.commandId == record.request.commandId }),
                  snapshot.pending[index].body == record.body else { throw PreviousActionIntentError.invalidRecord }
            snapshot.pending.remove(at: index)
            snapshot.completed.append(Completed(commandID: record.request.commandId,
                bodyDigest: Data(SHA256.hash(data: record.body)), dataEpoch: record.request.dataEpoch, response: response))
            for index in snapshot.completed.indices.dropLast(64) { snapshot.completed[index].response = nil }
            try files.save(snapshot)
        } }
    }

    static func sameIntent(_ lhs: ActionRequest, _ rhs: ActionRequest) -> Bool {
        lhs.protocolVersion == rhs.protocolVersion && lhs.dataEpoch == rhs.dataEpoch
            && lhs.actionId == rhs.actionId && lhs.optionId == rhs.optionId && lhs.input == rhs.input
    }
    static func validate(_ record: Record) throws {
        guard !record.request.commandId.isEmpty, !record.request.dataEpoch.isEmpty,
              record.body.count <= 256 * 1024, try ContractCodec.encode(record.restoredRequest) == record.body
        else { throw PreviousActionIntentError.invalidRecord }
    }
}

private final class PreviousIntentFiles: Sendable {
    private static let keyDomain = Data("alondra-action-intents-v1".utf8)
    private let queue = DispatchQueue(label: "com.bitomule.alondra.action-intents")
    private let directory: URL
    private let file: URL
    private let marker: URL
    private let lock: URL
    private let key: SymmetricKey
    private let binding: Data
    let completedLimit: Int

    init(directory: URL, keyMaterial: Data, scope: PreviousActionIntentScope, completedLimit: Int) throws {
        guard completedLimit > 0, keyMaterial.count == 32, !scope.service.isEmpty, !scope.device.isEmpty,
              !scope.credentialRevision.isEmpty else { throw PreviousActionIntentError.invalidScope }
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

    func run<T: Sendable>(_ work: @escaping @Sendable () throws -> T) async throws -> T {
        try await withCheckedThrowingContinuation { continuation in
            queue.async { continuation.resume(with: Result(catching: work)) }
        }
    }

    func locked<T>(_ work: () throws -> T) throws -> T {
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true,
                                                    attributes: [.posixPermissions: 0o700])
            let directoryFD = open(directory.path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
            guard directoryFD >= 0 else { throw PreviousActionIntentError.unreadableStore }
            defer { close(directoryFD) }
            let descriptor = open(lock.path, O_CREAT | O_RDWR | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK, 0o600)
            guard descriptor >= 0 else { throw PreviousActionIntentError.unreadableStore }
            defer { close(descriptor) }
            var lockInfo = stat()
            guard fstat(descriptor, &lockInfo) == 0, lockInfo.st_mode & S_IFMT == S_IFREG
            else { throw PreviousActionIntentError.unreadableStore }
            guard flock(descriptor, LOCK_EX) == 0 else { throw PreviousActionIntentError.unreadableStore }
            defer { _ = flock(descriptor, LOCK_UN) }
            return try work()
        } catch let error as PreviousActionIntentError { throw error }
        catch { throw PreviousActionIntentError.unreadableStore }
    }

    func load(initialize: Bool = false) throws -> PreviousActionIntentStore.Snapshot {
        let metadata = try read(marker, limit: 32)
        let ciphertext = try read(file, limit: PreviousActionIntentStore.maxBytes + 64)
        if metadata == nil && ciphertext == nil && initialize {
            let empty = PreviousActionIntentStore.Snapshot()
            try save(empty)
            try atomicWrite(Data(SHA256.hash(data: binding)), to: marker)
            let parent = open(directory.deletingLastPathComponent().path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
            guard parent >= 0 else { throw PreviousActionIntentError.unreadableStore }
            defer { close(parent) }
            guard fsync(parent) == 0 else { throw PreviousActionIntentError.unreadableStore }
            return empty
        }
        guard metadata == Data(SHA256.hash(data: binding)), let ciphertext else { throw PreviousActionIntentError.unreadableStore }
        do {
            let plaintext = try AES.GCM.open(AES.GCM.SealedBox(combined: ciphertext), using: key, authenticating: binding)
            let snapshot = try JSONDecoder().decode(PreviousActionIntentStore.Snapshot.self, from: plaintext)
            guard snapshot.pending.count <= PreviousActionIntentStore.maxPending,
                  snapshot.pending.count + snapshot.completed.count <= completedLimit,
                  Set(snapshot.pending.map { $0.request.commandId } + snapshot.completed.map(\.commandID)).count
                    == snapshot.pending.count + snapshot.completed.count else { throw PreviousActionIntentError.unreadableStore }
            for record in snapshot.pending { try PreviousActionIntentStore.validate(record) }
            guard snapshot.completed.allSatisfy({ !$0.commandID.isEmpty && !$0.dataEpoch.isEmpty && $0.bodyDigest.count == 32 })
            else { throw PreviousActionIntentError.unreadableStore }
            return snapshot
        } catch { throw PreviousActionIntentError.unreadableStore }
    }

    func save(_ snapshot: PreviousActionIntentStore.Snapshot) throws {
        let encoder = JSONEncoder(); encoder.outputFormatting = [.sortedKeys]
        var trimmed = snapshot
        var plaintext = try encoder.encode(trimmed)
        for index in trimmed.completed.indices where plaintext.count > PreviousActionIntentStore.maxBytes && trimmed.completed[index].response != nil {
            trimmed.completed[index].response = nil
            plaintext = try encoder.encode(trimmed)
        }
        guard plaintext.count <= PreviousActionIntentStore.maxBytes else { throw PreviousActionIntentError.capacityExceeded }
        let sealed = try AES.GCM.seal(plaintext, using: key, authenticating: binding)
        guard let ciphertext = sealed.combined else { throw PreviousActionIntentError.unreadableStore }
        try atomicWrite(ciphertext, to: file)
    }

    private func read(_ url: URL, limit: Int) throws -> Data? {
        let descriptor = open(url.path, O_RDONLY | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK)
        if descriptor < 0 {
            if errno == ENOENT { return nil }
            throw PreviousActionIntentError.unreadableStore
        }
        defer { close(descriptor) }
        var metadata = stat()
        guard fstat(descriptor, &metadata) == 0, metadata.st_mode & S_IFMT == S_IFREG,
              metadata.st_size >= 0, metadata.st_size <= limit else { throw PreviousActionIntentError.unreadableStore }
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
        guard complete else { throw PreviousActionIntentError.unreadableStore }
        return bytes
    }

    private func atomicWrite(_ bytes: Data, to destination: URL) throws {
        let temporary = directory.appending(path: UUID().uuidString + ".pending-write")
        let descriptor = open(temporary.path, O_CREAT | O_EXCL | O_WRONLY | O_NOFOLLOW | O_CLOEXEC, 0o600)
        guard descriptor >= 0 else { throw PreviousActionIntentError.unreadableStore }
        defer { close(descriptor); _ = unlink(temporary.path) }
        let complete = bytes.withUnsafeBytes { buffer -> Bool in
            var offset = 0
            while offset < buffer.count {
                let count = Darwin.write(descriptor, buffer.baseAddress!.advanced(by: offset), buffer.count - offset)
                if count < 0 && errno == EINTR { continue }
                guard count > 0 else { return false }
                offset += count
            }
            return true
        }
        guard complete, fsync(descriptor) == 0, rename(temporary.path, destination.path) == 0
        else { throw PreviousActionIntentError.unreadableStore }
        let directoryFD = open(directory.path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard directoryFD >= 0 else { throw PreviousActionIntentError.unreadableStore }
        defer { close(directoryFD) }
        guard fsync(directoryFD) == 0 else { throw PreviousActionIntentError.unreadableStore }
    }
}
