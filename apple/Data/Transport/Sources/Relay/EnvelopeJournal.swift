import CryptoKit
import Darwin
import Foundation
import SQLite3

public enum EnvelopeJournalError: Error, Sendable, Equatable {
    case unavailable, corrupt, wrongScope, duplicateRequestID
}

/// One pairing namespace. Blocking disk operations run on a dedicated serial queue; SQLite
/// transactions also serialize writers from another app process using this namespace.
public final class EnvelopeJournal: Sendable {
    private static let schema = 1
    let scope: RelayOutboxScope
    private let directory: URL
    private let queue = DispatchQueue(label: "alondra.operation-journal")

    public init(scope: RelayOutboxScope, directory: URL) throws {
        guard directory.isFileURL else { throw EnvelopeJournalError.unavailable }
        self.scope = scope
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        let digest = SHA256.hash(data: try encoder.encode(scope)).map { String(format: "%02x", $0) }.joined()
        self.directory = directory.appendingPathComponent(digest, isDirectory: true)
    }

    public func entry(requestID: String) async throws -> RelayOutboxEntry? {
        try await perform { database in try Self.read(database, id: requestID, scope: self.scope) }
    }

    public func entries() async throws -> [RelayOutboxEntry] {
        try await perform { database in
            let statement = try Self.prepare(database, "SELECT entry FROM operations ORDER BY request_id")
            defer { sqlite3_finalize(statement) }
            var entries: [RelayOutboxEntry] = []
            while true {
                switch sqlite3_step(statement) {
                case SQLITE_ROW:
                    let entry = try Self.decode(statement, scope: self.scope)
                    entries.append(entry)
                case SQLITE_DONE: return entries
                default: throw EnvelopeJournalError.unavailable
                }
            }
        }
    }

    func reserve(_ entry: RelayOutboxEntry) async throws {
        try await perform { database in
            guard entry.scope == self.scope else { throw EnvelopeJournalError.wrongScope }
            guard try Self.read(database, id: entry.requestID, scope: self.scope) == nil else {
                throw EnvelopeJournalError.duplicateRequestID
            }
            try Self.write(database, entry: entry)
        }
    }

    func update(requestID: String, _ change: @escaping @Sendable (inout RelayOutboxEntry) throws -> Void) async throws -> RelayOutboxEntry {
        try await perform { database in
            guard var entry = try Self.read(database, id: requestID, scope: self.scope) else {
                throw EnvelopeJournalError.corrupt
            }
            try change(&entry)
            guard entry.requestID == requestID, entry.scope == self.scope else { throw EnvelopeJournalError.wrongScope }
            try Self.write(database, entry: entry)
            return entry
        }
    }

    private func perform<Result: Sendable>(_ operation: @escaping @Sendable (OpaquePointer) throws -> Result) async throws -> Result {
        try await withCheckedThrowingContinuation { continuation in
            queue.async {
                do {
                    let database = try self.open()
                    defer { sqlite3_close(database) }
                    try Self.execute(database, "BEGIN IMMEDIATE")
                    do {
                        let value = try operation(database)
                        try Self.execute(database, "COMMIT")
                        continuation.resume(returning: value)
                    } catch {
                        try? Self.execute(database, "ROLLBACK")
                        throw error
                    }
                } catch { continuation.resume(throwing: error) }
            }
        }
    }

    private func open() throws -> OpaquePointer {
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true,
                                                attributes: [.posixPermissions: 0o700])
        var namespace = stat()
        guard lstat(directory.path, &namespace) == 0, (namespace.st_mode & S_IFMT) == S_IFDIR else {
            throw EnvelopeJournalError.corrupt
        }
        let file = directory.appendingPathComponent("operations.sqlite")
        let marker = directory.appendingPathComponent("initialized")
        var info = stat()
        let exists = lstat(file.path, &info) == 0
        if !exists, errno != ENOENT { throw EnvelopeJournalError.unavailable }
        if exists, (info.st_mode & S_IFMT) != S_IFREG { throw EnvelopeJournalError.corrupt }
        if !exists, FileManager.default.fileExists(atPath: marker.path) { throw EnvelopeJournalError.corrupt }
        var opened: OpaquePointer?
        let flags = SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE | SQLITE_OPEN_FULLMUTEX | SQLITE_OPEN_NOFOLLOW
        guard sqlite3_open_v2(file.path, &opened, flags, nil) == SQLITE_OK, let database = opened else {
            if let opened { sqlite3_close(opened) }
            throw EnvelopeJournalError.unavailable
        }
        do {
            guard chmod(file.path, 0o600) == 0 else { throw EnvelopeJournalError.unavailable }
            sqlite3_busy_timeout(database, 1_000)
            try Self.execute(database, "PRAGMA synchronous=FULL")
            try Self.execute(database, "PRAGMA journal_mode=DELETE")
            try Self.execute(database, "BEGIN IMMEDIATE")
            do {
                let schema = try Self.integer(database, "PRAGMA user_version")
                if schema == 0, !FileManager.default.fileExists(atPath: marker.path) {
                    guard try Self.integer(database, "SELECT count(*) FROM sqlite_master WHERE type='table'") == 0 else {
                        throw EnvelopeJournalError.corrupt
                    }
                    try Self.execute(database, "CREATE TABLE journal_scope(scope BLOB NOT NULL)")
                    try Self.execute(database, "CREATE TABLE operations(request_id TEXT PRIMARY KEY NOT NULL, entry BLOB NOT NULL)")
                    let insert = try Self.prepare(database, "INSERT INTO journal_scope(scope) VALUES(?)")
                    defer { sqlite3_finalize(insert) }
                    try Self.bind(try JSONEncoder().encode(scope), statement: insert, index: 1)
                    guard sqlite3_step(insert) == SQLITE_DONE else { throw EnvelopeJournalError.unavailable }
                    try Self.execute(database, "PRAGMA user_version=\(Self.schema)")
                } else if schema != Self.schema { throw EnvelopeJournalError.corrupt }
                try Self.execute(database, "COMMIT")
            } catch { try? Self.execute(database, "ROLLBACK"); throw error }
            let stored = try Self.prepare(database, "SELECT scope FROM journal_scope")
            defer { sqlite3_finalize(stored) }
            guard sqlite3_step(stored) == SQLITE_ROW,
                  try JSONDecoder().decode(RelayOutboxScope.self, from: Self.bytes(stored)) == scope,
                  sqlite3_step(stored) == SQLITE_DONE else { throw EnvelopeJournalError.wrongScope }
            var initialized = false
            if !FileManager.default.fileExists(atPath: marker.path) {
                let descriptor = Darwin.open(marker.path, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, 0o600)
                if descriptor >= 0 {
                    initialized = true
                    defer { Darwin.close(descriptor) }
                    var byte: UInt8 = 1
                    guard Darwin.write(descriptor, &byte, 1) == 1, Darwin.fsync(descriptor) == 0 else { throw EnvelopeJournalError.unavailable }
                } else if errno != EEXIST { throw EnvelopeJournalError.unavailable }
            }
            var markerInfo = stat()
            guard lstat(marker.path, &markerInfo) == 0, (markerInfo.st_mode & S_IFMT) == S_IFREG else {
                throw EnvelopeJournalError.corrupt
            }
            if initialized || !exists {
                for path in [directory, directory.deletingLastPathComponent()] {
                    let descriptor = Darwin.open(path.path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW)
                    guard descriptor >= 0 else { throw EnvelopeJournalError.unavailable }
                    defer { Darwin.close(descriptor) }
                    guard Darwin.fsync(descriptor) == 0 else { throw EnvelopeJournalError.unavailable }
                }
            }
            return database
        } catch { sqlite3_close(database); throw error }
    }

    private static func read(_ database: OpaquePointer, id: String, scope: RelayOutboxScope) throws -> RelayOutboxEntry? {
        let statement = try prepare(database, "SELECT entry FROM operations WHERE request_id=?")
        defer { sqlite3_finalize(statement) }
        try bind(Data(id.utf8), statement: statement, index: 1, text: true)
        switch sqlite3_step(statement) {
        case SQLITE_DONE: return nil
        case SQLITE_ROW:
            let entry = try decode(statement, scope: scope)
            guard entry.requestID == id else { throw EnvelopeJournalError.corrupt }
            return entry
        default: throw EnvelopeJournalError.unavailable
        }
    }

    private static func decode(_ statement: OpaquePointer, scope: RelayOutboxScope) throws -> RelayOutboxEntry {
        let entry = try JSONDecoder().decode(RelayOutboxEntry.self, from: bytes(statement))
        guard entry.scope == scope else { throw EnvelopeJournalError.wrongScope }
        return entry
    }

    private static func write(_ database: OpaquePointer, entry: RelayOutboxEntry) throws {
        let statement = try prepare(database, "INSERT INTO operations(request_id,entry) VALUES(?,?) ON CONFLICT(request_id) DO UPDATE SET entry=excluded.entry")
        defer { sqlite3_finalize(statement) }
        try bind(Data(entry.requestID.utf8), statement: statement, index: 1, text: true)
        try bind(try JSONEncoder().encode(entry), statement: statement, index: 2)
        guard sqlite3_step(statement) == SQLITE_DONE else { throw EnvelopeJournalError.unavailable }
    }

    private static func bytes(_ statement: OpaquePointer) throws -> Data {
        let count = sqlite3_column_bytes(statement, 0)
        guard count > 0, count <= 2 * 1024 * 1024, let data = sqlite3_column_blob(statement, 0) else {
            throw EnvelopeJournalError.corrupt
        }
        return Data(bytes: data, count: Int(count))
    }

    private static func bind(_ data: Data, statement: OpaquePointer, index: Int32, text: Bool = false) throws {
        guard data.count <= 2 * 1024 * 1024 else { throw EnvelopeJournalError.unavailable }
        let transient = unsafeBitCast(-1, to: sqlite3_destructor_type.self)
        let code = data.withUnsafeBytes { bytes in
            text ? sqlite3_bind_text(statement, index, bytes.baseAddress?.assumingMemoryBound(to: CChar.self), Int32(bytes.count), transient)
                 : sqlite3_bind_blob(statement, index, bytes.baseAddress, Int32(bytes.count), transient)
        }
        guard code == SQLITE_OK else { throw EnvelopeJournalError.unavailable }
    }

    private static func integer(_ database: OpaquePointer, _ sql: String) throws -> Int {
        let statement = try prepare(database, sql)
        defer { sqlite3_finalize(statement) }
        guard sqlite3_step(statement) == SQLITE_ROW else { throw EnvelopeJournalError.corrupt }
        return Int(sqlite3_column_int(statement, 0))
    }

    private static func prepare(_ database: OpaquePointer, _ sql: String) throws -> OpaquePointer {
        var statement: OpaquePointer?
        guard sqlite3_prepare_v2(database, sql, -1, &statement, nil) == SQLITE_OK, let statement else {
            throw EnvelopeJournalError.unavailable
        }
        return statement
    }

    private static func execute(_ database: OpaquePointer, _ sql: String) throws {
        guard sqlite3_exec(database, sql, nil, nil, nil) == SQLITE_OK else { throw EnvelopeJournalError.unavailable }
    }
}
