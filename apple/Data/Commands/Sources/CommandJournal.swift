import Foundation
import Darwin

@_silgen_name("flock")
private func fileLock(_ descriptor: Int32, _ operation: Int32) -> Int32

/// Metadata only. `CommandKind` associated strings are target identifiers; callers must
/// never put a message body, attachment, signed request, or credential in them.
public enum CommandJournalState: String, Codable, Sendable {
    case pending
    case confirmed
    case unknown
}

public struct CommandJournalRecord: Codable, Sendable {
    public let command: UncertainCommand
    public let state: CommandJournalState

    public init(command: UncertainCommand, state: CommandJournalState) {
        self.command = command
        self.state = state
    }
}

public enum CommandJournalReservation: Sendable {
    case reserved
    case existing(UncertainCommand)
}

/// The caller supplies a container-specific Application Support directory. An extension
/// may use its own container or an explicitly provisioned App Group; no global path is used.
public protocol CommandJournaling: Sendable {
    func records() async throws -> [CommandJournalRecord]
    func reserve(_ command: UncertainCommand) async throws -> CommandJournalReservation
    func save(_ record: CommandJournalRecord) async throws
    func remove(_ id: UUID) async throws
}

public actor FileCommandJournal: CommandJournaling {
    private let directory: URL
    private let manager = FileManager.default

    public init(directory: URL) { self.directory = directory }

    public func records() throws -> [CommandJournalRecord] {
        try withLock { try recordsUnlocked() }
    }

    public func reserve(_ command: UncertainCommand) throws -> CommandJournalReservation {
        try withLock {
            if let existing = try recordsUnlocked().first(where: {
                ($0.state == .pending || $0.state == .unknown)
                    && $0.command.conflicts(with: command.kind, itemID: command.itemID, scope: command.scope)
            }) {
                return .existing(existing.command)
            }
            try saveUnlocked(.init(command: command, state: .pending))
            return .reserved
        }
    }

    public func save(_ record: CommandJournalRecord) throws {
        try withLock { try saveUnlocked(record) }
    }

    public func remove(_ id: UUID) throws {
        try withLock {
            let file = directory.appendingPathComponent(id.uuidString).appendingPathExtension("json")
            guard manager.fileExists(atPath: file.path) else { return }
            try manager.removeItem(at: file)
            try syncDirectory()
        }
    }

    private func recordsUnlocked() throws -> [CommandJournalRecord] {
        return try manager.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil)
            .filter { $0.pathExtension == "json" }
            .map { try JSONDecoder().decode(CommandJournalRecord.self, from: Data(contentsOf: $0)) }
    }

    private func saveUnlocked(_ record: CommandJournalRecord) throws {
        guard record.command.scope.server.user == nil,
              record.command.scope.server.password == nil,
              record.command.scope.server.query == nil,
              record.command.scope.server.fragment == nil else {
            throw CommandJournalError.secretBearingServerURL
        }
        let file = directory.appendingPathComponent(record.command.id.uuidString).appendingPathExtension("json")
        let data = try JSONEncoder().encode(record)
        let temporary = directory.appendingPathComponent(".\(UUID().uuidString).tmp")
        let descriptor = Darwin.open(temporary.path, O_WRONLY | O_CREAT | O_EXCL, 0o600)
        guard descriptor >= 0 else { throw posixError() }
        var isOpen = true
        do {
            try data.withUnsafeBytes { bytes in
                var offset = 0
                while offset < bytes.count {
                    let written = Darwin.write(descriptor, bytes.baseAddress!.advanced(by: offset), bytes.count - offset)
                    if written < 0 && errno == EINTR { continue }
                    guard written > 0 else { throw posixError() }
                    offset += written
                }
            }
            guard Darwin.fsync(descriptor) == 0 else { throw posixError() }
            guard Darwin.close(descriptor) == 0 else { throw posixError() }
            isOpen = false
            guard Darwin.rename(temporary.path, file.path) == 0 else { throw posixError() }
            if record.state == .pending {
                try syncDirectory() // Required before the transport may be called.
            } else {
                // A previous pending record is durable. If directory sync fails here,
                // either terminal state or pending state can survive; both are safe.
                _ = try? syncDirectory()
                if record.state == .confirmed {
                    // Terminal metadata has no reconciliation value. A failed cleanup only
                    // leaves a harmless confirmed record, never an executable request.
                    do {
                        try manager.removeItem(at: file)
                        _ = try? syncDirectory()
                    } catch {}
                }
            }
        } catch {
            if isOpen { _ = Darwin.close(descriptor) }
            _ = Darwin.unlink(temporary.path)
            throw error
        }
    }

    private func withLock<T>(_ body: () throws -> T) throws -> T {
        try manager.createDirectory(at: directory, withIntermediateDirectories: true,
            attributes: [.posixPermissions: 0o700])
        try manager.setAttributes([.posixPermissions: 0o700], ofItemAtPath: directory.path)
        let lock = directory.appendingPathComponent(".journal.lock")
        let descriptor = Darwin.open(lock.path, O_RDWR | O_CREAT, 0o600)
        guard descriptor >= 0 else { throw posixError() }
        defer { _ = fileLock(descriptor, LOCK_UN); _ = Darwin.close(descriptor) }
        while fileLock(descriptor, LOCK_EX) != 0 {
            guard errno == EINTR else { throw posixError() }
        }
        return try body()
    }

    private func syncDirectory() throws {
        let descriptor = Darwin.open(directory.path, O_RDONLY)
        guard descriptor >= 0 else { throw posixError() }
        defer { _ = Darwin.close(descriptor) }
        guard Darwin.fsync(descriptor) == 0 else { throw posixError() }
    }

    private func posixError() -> POSIXError {
        POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
    }
}

public enum CommandJournalError: Error, Sendable {
    case secretBearingServerURL
}
