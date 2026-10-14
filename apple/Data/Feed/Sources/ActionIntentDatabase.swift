import AlondraContract
import AlondraTransport
import CryptoKit
import Darwin
import Foundation
import SQLite3

/// The counter and each intent are committed together; disk work never occupies a Swift actor executor.
final class ActionIntentDatabase: Sendable {
    typealias Interval = ViewActionIntentStore.RetiredInterval
    static let maxCounter = ActionOperationID.maximumCounter
    struct Metadata: Codable, Sendable {
        var generation: UInt64
        var next: UInt64
        var activeEpoch: String
        var retired: [Interval]
        var acknowledged: [Interval]
        /// The first sequence of every generation. Above 1 only for a store that replaced one set
        /// aside: the Mac may still hold that store's sequences, and a reused one never completes.
        var floor: UInt64
        init(generation: UInt64 = 0, next: UInt64? = nil, activeEpoch: String = "", retired: [Interval] = [],
             acknowledged: [Interval] = [], floor: UInt64 = 1) {
            self.generation = generation; self.next = next ?? floor; self.activeEpoch = activeEpoch
            self.retired = retired; self.acknowledged = acknowledged; self.floor = floor
        }
        private enum CodingKeys: String, CodingKey { case generation, next, activeEpoch, retired, acknowledged, floor }
        init(from decoder: any Decoder) throws {
            let values = try decoder.container(keyedBy: CodingKeys.self)
            generation = try values.decode(UInt64.self, forKey: .generation)
            next = try values.decode(UInt64.self, forKey: .next)
            activeEpoch = try values.decode(String.self, forKey: .activeEpoch)
            retired = try values.decode([Interval].self, forKey: .retired)
            acknowledged = try values.decodeIfPresent([Interval].self, forKey: .acknowledged) ?? []
            floor = try values.decodeIfPresent(UInt64.self, forKey: .floor) ?? 1
        }
    }
    typealias Ticket = ActionOperationID
    private static func validNonce(_ value: String) -> Bool {
        guard let uuid = UUID(uuidString: value), uuid.uuidString.lowercased() == value else { return false }
        return uuid.uuid.6 >> 4 == 4 && uuid.uuid.8 & 0xc0 == 0x80
    }
    private let directory: URL
    private let file: URL
    private let marker: URL
    private let legacy: LegacyIntentFiles
    private let key: SymmetricKey
    private let indexKey: SymmetricKey
    private let binding: Data
    private let legacyLimit: Int
    private let sequenceFloor: UInt64
    /// Every file of this identity's store starts with it, in both the SQLite and the legacy format.
    let name: String
    private let queue = DispatchQueue(label: "alondra.action-intent-database")
    private static var transient: sqlite3_destructor_type { unsafeBitCast(-1, to: sqlite3_destructor_type.self) }

    init(directory: URL, keyMaterial: Data, scope: ActionIntentScope, legacyLimit: Int, sequenceFloor: UInt64 = 1) throws {
        guard (1...Self.maxCounter).contains(sequenceFloor) else { throw ViewActionIntentError.invalidScope }
        self.sequenceFloor = sequenceFloor
        legacy = try LegacyIntentFiles(directory: directory, keyMaterial: keyMaterial, scope: scope, completedLimit: legacyLimit)
        let encoder = JSONEncoder(); encoder.outputFormatting = [.sortedKeys]
        binding = try encoder.encode(scope)
        key = HKDF<SHA256>.deriveKey(inputKeyMaterial: SymmetricKey(data: keyMaterial), salt: binding,
            info: Data("alondra-action-intent-rows-v1".utf8), outputByteCount: 32)
        indexKey = HKDF<SHA256>.deriveKey(inputKeyMaterial: SymmetricKey(data: keyMaterial), salt: binding,
            info: Data("alondra-action-intent-index-v1".utf8), outputByteCount: 32)
        let name = SHA256.hash(data: binding).map { String(format: "%02x", $0) }.joined()
        self.directory = directory; self.legacyLimit = legacyLimit; self.name = name
        file = directory.appending(path: name + ".sqlite")
        marker = directory.appending(path: name + ".sqlite-initialized")
    }
    func perform<T: Sendable>(_ work: @escaping @Sendable (OpaquePointer) throws -> T) async throws -> T {
        try await withCheckedThrowingContinuation { continuation in
            queue.async {
                do {
                    let result = try self.legacy.locked {
                        let db = try self.open(); defer { sqlite3_close(db) }
                        try Self.execute(db, "BEGIN IMMEDIATE")
                        do {
                            let result = try work(db)
                            try Self.execute(db, "COMMIT")
                            return result
                        } catch { try? Self.execute(db, "ROLLBACK"); throw error }
                    }
                    continuation.resume(returning: result)
                } catch let error as ViewActionIntentError { continuation.resume(throwing: error) }
                  catch { continuation.resume(throwing: ViewActionIntentError.unreadableStore) }
            }
        }
    }
    /// SQLite's NOFOLLOW refuses a symbolic link anywhere in the path, and every iOS app container is
    /// reached through one (/var is /private/var). Only the database file itself must not be a link.
    private func databasePath() throws -> String {
        try resolvedDirectory().appending(path: file.lastPathComponent).path
    }
    private func resolvedDirectory() throws -> URL {
        guard let resolved = realpath(directory.path, nil) else { throw ViewActionIntentError.unreadableStore }
        defer { free(resolved) }
        return URL(fileURLWithPath: String(cString: resolved), isDirectory: true)
    }
    private func open() throws -> OpaquePointer {
        let path = try databasePath()
        var info = stat()
        let exists = lstat(path, &info) == 0
        guard exists || errno == ENOENT, !exists || info.st_mode & S_IFMT == S_IFREG else { throw ViewActionIntentError.unreadableStore }
        let initialization = try legacy.read(marker, limit: 32)
        let initialized = initialization != nil
        guard !initialized || (exists && initialization == Data(SHA256.hash(data: binding))) else { throw ViewActionIntentError.unreadableStore }
        var pointer: OpaquePointer?
        guard sqlite3_open_v2(path, &pointer, SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE | SQLITE_OPEN_FULLMUTEX | SQLITE_OPEN_NOFOLLOW, nil) == SQLITE_OK,
              let db = pointer else { if let pointer { sqlite3_close(pointer) }; throw ViewActionIntentError.unreadableStore }
        do {
            guard chmod(path, 0o600) == 0 else { throw ViewActionIntentError.unreadableStore }
            sqlite3_busy_timeout(db, 1_000)
            try Self.execute(db, "PRAGMA page_size=4096")
            try Self.execute(db, "PRAGMA max_page_count=4096")
            try Self.execute(db, "PRAGMA synchronous=FULL")
            try Self.execute(db, "PRAGMA journal_mode=DELETE")
            try Self.execute(db, "BEGIN IMMEDIATE")
            do {
                let schema = try Self.integer(db, "PRAGMA user_version")
                if schema == 0 {
                    guard !initialized, try Self.integer(db, "SELECT count(*) FROM sqlite_master WHERE type='table'") == 0 else { throw ViewActionIntentError.unreadableStore }
                    let snapshot = try legacy.existingSnapshot()
                    try Self.execute(db, "CREATE TABLE metadata(id INTEGER PRIMARY KEY CHECK(id=1), body BLOB NOT NULL)")
                    for table in ["pending", "legacy", "recent"] { try Self.execute(db, "CREATE TABLE \(table)(id BLOB PRIMARY KEY NOT NULL, body BLOB NOT NULL)") }
                    try writeMetadata(db, Metadata(floor: sequenceFloor))
                    if let snapshot {
                        for record in snapshot.pending { try put(db, table: "pending", id: record.request.commandId, value: record) }
                        for completed in snapshot.completed {
                            let proof = ViewActionIntentStore.Completed(commandID: completed.commandID, bodyDigest: completed.bodyDigest, dataEpoch: completed.dataEpoch, response: nil)
                            try put(db, table: "legacy", id: proof.commandID, value: proof)
                        }
                        for completed in snapshot.completed.suffix(64) where completed.response != nil {
                            try put(db, table: "recent", id: completed.commandID, value: completed)
                        }
                        try trimRecent(db)
                    }
                    try Self.execute(db, "PRAGMA user_version=1")
                } else { guard schema == 1 else { throw ViewActionIntentError.unreadableStore } }
                _ = try metadata(db)
                try Self.execute(db, "COMMIT")
            } catch { try? Self.execute(db, "ROLLBACK"); throw error }
            if !initialized {
                let descriptor = Darwin.open(marker.path, O_CREAT | O_EXCL | O_WRONLY | O_NOFOLLOW | O_CLOEXEC, 0o600)
                guard descriptor >= 0 else { throw ViewActionIntentError.unreadableStore }
                defer { Darwin.close(descriptor) }
                let digest = Data(SHA256.hash(data: binding))
                let written = digest.withUnsafeBytes { Darwin.write(descriptor, $0.baseAddress, $0.count) }
                guard written == digest.count, fsync(descriptor) == 0 else { throw ViewActionIntentError.unreadableStore }
                try syncDirectories()
                // The committed database is authoritative before removing the old ciphertext.
                if unlink(legacy.file.path) == 0 { try syncDirectories() }
            }
            var markerInfo = stat()
            guard lstat(marker.path, &markerInfo) == 0, markerInfo.st_mode & S_IFMT == S_IFREG,
                  try legacy.read(marker, limit: 32) == Data(SHA256.hash(data: binding)) else { throw ViewActionIntentError.unreadableStore }
            return db
        } catch { sqlite3_close(db); throw error }
    }
    private func syncDirectories() throws {
        let resolved = try resolvedDirectory()
        for path in [resolved, resolved.deletingLastPathComponent()] {
            let fd = Darwin.open(path.path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
            guard fd >= 0 else { throw ViewActionIntentError.unreadableStore }
            defer { Darwin.close(fd) }
            guard fsync(fd) == 0 else { throw ViewActionIntentError.unreadableStore }
        }
    }
    /// Reads what it still can of the answers the store held, then moves every file of it into
    /// `folder`. Nothing read here is ever sent: the store could not prove what the Mac has.
    func setAside(into folder: URL) async throws -> SetAsideAnswers {
        try await withCheckedThrowingContinuation { continuation in
            queue.async {
                do {
                    let answers = try self.legacy.locked {
                        let answers = self.salvage()
                        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true,
                                                                attributes: [.posixPermissions: 0o700])
                        for entry in try FileManager.default.contentsOfDirectory(atPath: self.directory.path)
                        where entry.hasPrefix(self.name) && !entry.hasSuffix(".lock") {
                            try FileManager.default.moveItem(at: self.directory.appending(path: entry), to: folder.appending(path: entry))
                        }
                        try self.syncDirectories()
                        return answers
                    }
                    continuation.resume(returning: answers)
                } catch { continuation.resume(throwing: error) }
            }
        }
    }
    private func salvage() -> SetAsideAnswers {
        if let path = try? databasePath(), FileManager.default.fileExists(atPath: path) {
            var complete = true
            let records = readablePending(at: path, complete: &complete)
            return SetAsideAnswers(requests: records.map(\.restoredRequest), complete: complete)
        }
        if FileManager.default.fileExists(atPath: marker.path) { return SetAsideAnswers(requests: [], complete: false) }
        do { return SetAsideAnswers(requests: (try legacy.existingSnapshot()?.pending ?? []).map(\.restoredRequest), complete: true) }
        catch { return SetAsideAnswers(requests: [], complete: false) }
    }
    private func readablePending(at path: String, complete: inout Bool) -> [ViewActionIntentStore.Record] {
        var pointer: OpaquePointer?
        guard sqlite3_open_v2(path, &pointer, SQLITE_OPEN_READONLY | SQLITE_OPEN_NOFOLLOW, nil) == SQLITE_OK, let db = pointer,
              let statement = try? Self.prepare(db, "SELECT body,id FROM pending ORDER BY rowid") else {
            sqlite3_close(pointer); complete = false; return []
        }
        defer { sqlite3_finalize(statement); sqlite3_close(db) }
        var records: [ViewActionIntentStore.Record] = []
        while records.count < ViewActionIntentStore.maxPending {
            switch sqlite3_step(statement) {
            case SQLITE_DONE: return records
            case SQLITE_ROW:
                guard let digest = try? Self.bytes(statement, column: 1), let body = try? Self.bytes(statement),
                      let record: ViewActionIntentStore.Record = try? decrypt(body, context: Data("pending".utf8) + digest),
                      identifier(record.request.commandId) == digest else { complete = false; continue }
                records.append(record)
            default: complete = false; return records
            }
        }
        complete = false
        return records
    }
    func pending(_ db: OpaquePointer) throws -> [ViewActionIntentStore.Record] {
        let values: [ViewActionIntentStore.Record] = try all(db, table: "pending")
        guard values.count <= ViewActionIntentStore.maxPending else { throw ViewActionIntentError.unreadableStore }
        for record in values { try ViewActionIntentStore.validate(record) }
        return values
    }
    func prepareNew(_ db: OpaquePointer, request: ActionRequest, generation: UInt64) throws -> ViewActionIntentStore.Record {
        guard generation > 0, generation <= Self.maxCounter, Self.validNonce(request.commandId), !request.dataEpoch.isEmpty else { throw ViewActionIntentError.invalidRecord }
        var entries = try pending(db)
        var state = try metadata(db)
        guard generation >= state.generation else { throw ViewActionIntentError.invalidRecord }
        if generation > state.generation {
            for record in entries where Ticket(rawValue: record.request.commandId) != nil {
                try remove(db, table: "pending", id: record.request.commandId)
            }
            let recent: [ViewActionIntentStore.Completed] = try all(db, table: "recent")
            for completed in recent where Ticket(rawValue: completed.commandID) != nil { try remove(db, table: "recent", id: completed.commandID) }
            state = Metadata(generation: generation, activeEpoch: request.dataEpoch, floor: state.floor)
            try writeMetadata(db, state)
            entries = entries.filter { Ticket(rawValue: $0.request.commandId) == nil }
        }
        guard state.activeEpoch == request.dataEpoch else { throw ViewActionIntentError.invalidRecord }
        if let existing = entries.first(where: {
            ViewActionIntentStore.sameIntent($0.request, request)
                && (Ticket(rawValue: $0.request.commandId)?.generation == generation || Ticket(rawValue: $0.request.commandId) == nil)
        }) { return existing }
        let outstanding = UInt64(entries.count) + Self.count(state.retired) - Self.count(state.acknowledged)
        guard outstanding < UInt64(ViewActionIntentStore.maxPending), state.next <= Self.maxCounter else { throw ViewActionIntentError.capacityExceeded }
        guard let operation = Ticket(dataEpoch: request.dataEpoch, generation: generation, sequence: state.next, nonce: request.commandId) else { throw ViewActionIntentError.invalidRecord }
        let id = operation.rawValue
        let prepared = ActionRequest(protocolVersion: request.protocolVersion, commandId: id, dataEpoch: request.dataEpoch,
            client: request.client, lang: request.lang, actionId: request.actionId, optionId: request.optionId,
            input: request.input, resolvesCard: request.resolvesItsCard)
        let record = ViewActionIntentStore.Record(request: prepared, body: try ContractCodec.encode(prepared), phase: .prepared)
        try ViewActionIntentStore.validate(record)
        state.next += 1
        try put(db, table: "pending", id: id, value: record)
        try requirePendingBudget(db)
        try writeMetadata(db, state)
        return record
    }
    func reserve(_ db: OpaquePointer, request: ActionRequest, body: Data, allowLegacy: Bool) throws -> ViewActionIntentStore.Reservation {
        let supplied = ViewActionIntentStore.Record(request: request, body: body)
        try ViewActionIntentStore.validate(supplied)
        if let completed: ViewActionIntentStore.Completed = try get(db, table: "recent", id: request.commandId)
            ?? get(db, table: "legacy", id: request.commandId) {
            guard completed.bodyDigest == Data(SHA256.hash(data: body)), completed.dataEpoch == request.dataEpoch else { throw ViewActionIntentError.invalidRecord }
            return .completed(completed.response)
        }
        if let existing: ViewActionIntentStore.Record = try get(db, table: "pending", id: request.commandId) {
            guard existing.body == body else { throw ViewActionIntentError.invalidRecord }
            if existing.phase != .prepared { return .pending(existing) }
            let claimed = ViewActionIntentStore.Record(request: existing.restoredRequest, body: existing.body)
            try put(db, table: "pending", id: request.commandId, value: claimed)
            return .created(claimed)
        }
        if let ticket = Ticket(rawValue: request.commandId) {
            guard ticket.epochTag == Ticket.tag(for: request.dataEpoch) else { throw ViewActionIntentError.invalidRecord }
            let state = try metadata(db)
            if ticket.generation < state.generation || (ticket.generation == state.generation && state.retired.contains(where: { $0.first <= ticket.sequence && ticket.sequence <= $0.last })) {
                return .completed(nil)
            }
            throw ViewActionIntentError.invalidRecord
        }
        guard !request.commandId.hasPrefix("op_"), Self.validNonce(request.commandId) else { throw ViewActionIntentError.invalidRecord }
        let entries = try pending(db)
        if let existing = entries.first(where: { ViewActionIntentStore.sameIntent($0.request, request) }) { return .pending(existing) }
        guard allowLegacy else { throw ViewActionIntentError.invalidRecord }
        guard entries.count < ViewActionIntentStore.maxPending,
              try Self.integer(db, "SELECT count(*) FROM legacy") + entries.filter({ Ticket(rawValue: $0.request.commandId) == nil }).count < legacyLimit else { throw ViewActionIntentError.capacityExceeded }
        let reservedLegacy = try entries.filter { Ticket(rawValue: $0.request.commandId) == nil }.reduce(0) { try $0 + proofBytes($1) }
        guard try Self.integer(db, "SELECT coalesce(sum(length(body)),0) FROM legacy") + reservedLegacy + proofBytes(supplied) <= 5 * 1024 * 1024 else { throw ViewActionIntentError.capacityExceeded }
        try put(db, table: "pending", id: request.commandId, value: supplied)
        try requirePendingBudget(db)
        return .created(supplied)
    }
    func complete(_ db: OpaquePointer, record: ViewActionIntentStore.Record, response: ActionResponse) throws {
        guard let existing: ViewActionIntentStore.Record = try get(db, table: "pending", id: record.request.commandId) else {
            let previous: ViewActionIntentStore.Completed? = try get(db, table: "recent", id: record.request.commandId) ?? get(db, table: "legacy", id: record.request.commandId)
            if previous != nil { return }
            if let ticket = Ticket(rawValue: record.request.commandId) {
                let state = try metadata(db)
                if ticket.generation < state.generation || (ticket.generation == state.generation && state.retired.contains(where: { $0.first <= ticket.sequence && ticket.sequence <= $0.last })) { return }
            }
            throw ViewActionIntentError.invalidRecord
        }
        guard existing.body == record.body else { throw ViewActionIntentError.invalidRecord }
        let completed = ViewActionIntentStore.Completed(commandID: record.request.commandId,
            bodyDigest: Data(SHA256.hash(data: record.body)), dataEpoch: record.request.dataEpoch, response: response)
        if let ticket = Ticket(rawValue: record.request.commandId) {
            var state = try metadata(db)
            guard ticket.generation <= state.generation else { throw ViewActionIntentError.invalidRecord }
            if ticket.generation == state.generation {
                state.retired.append(Interval(first: ticket.sequence, last: ticket.sequence))
                state.retired.sort { $0.first < $1.first }
                var merged: [Interval] = []
                for range in state.retired {
                    if let last = merged.last, range.first <= last.last + 1 { merged[merged.count - 1].last = max(last.last, range.last) }
                    else { merged.append(range) }
                }
                state.retired = merged
                guard merged.count <= ViewActionIntentStore.maxPending + 1 else { throw ViewActionIntentError.unreadableStore }
                try writeMetadata(db, state)
            }
        } else {
            let proof = ViewActionIntentStore.Completed(commandID: completed.commandID, bodyDigest: completed.bodyDigest,
                dataEpoch: completed.dataEpoch, response: nil)
            try put(db, table: "legacy", id: completed.commandID, value: proof)
        }
        try remove(db, table: "pending", id: completed.commandID)
        try put(db, table: "recent", id: completed.commandID, value: completed)
        try trimRecent(db)
    }
    /// The same answer under the next operation ID of its generation; the old ID is retired, so
    /// once the Mac is told it refuses that one for good.
    func reissue(_ db: OpaquePointer, record: ViewActionIntentStore.Record) throws -> ViewActionIntentStore.Record {
        guard let existing: ViewActionIntentStore.Record = try get(db, table: "pending", id: record.request.commandId),
              existing.body == record.body, let ticket = Ticket(rawValue: record.request.commandId, dataEpoch: record.request.dataEpoch)
        else { throw ViewActionIntentError.invalidRecord }
        var state = try metadata(db)
        guard ticket.generation == state.generation, state.activeEpoch == record.request.dataEpoch, state.next <= Self.maxCounter,
              let operation = Ticket(dataEpoch: record.request.dataEpoch, generation: state.generation, sequence: state.next, nonce: ticket.nonce)
        else { throw ViewActionIntentError.invalidRecord }
        let entries = try pending(db)
        let outstanding = UInt64(entries.count) + Self.count(state.retired) - Self.count(state.acknowledged)
        guard outstanding < UInt64(ViewActionIntentStore.maxPending) else { throw ViewActionIntentError.capacityExceeded }
        let original = existing.restoredRequest
        let next = ActionRequest(protocolVersion: original.protocolVersion, commandId: operation.rawValue, dataEpoch: original.dataEpoch,
            client: original.client, lang: original.lang, actionId: original.actionId, optionId: original.optionId,
            input: original.input, resolvesCard: original.resolvesItsCard)
        let replacement = ViewActionIntentStore.Record(request: next, body: try ContractCodec.encode(next), phase: .prepared)
        try ViewActionIntentStore.validate(replacement)
        state.retired = Self.merged(state.retired + [Interval(first: ticket.sequence, last: ticket.sequence)])
        guard state.retired.count <= ViewActionIntentStore.maxPending + 1 else { throw ViewActionIntentError.unreadableStore }
        state.next += 1
        try remove(db, table: "pending", id: record.request.commandId)
        try put(db, table: "pending", id: replacement.request.commandId, value: replacement)
        try requirePendingBudget(db)
        try writeMetadata(db, state)
        return replacement
    }

    /// An answer that can never be delivered leaves the queue; a ticketed ID is retired with it.
    func discard(_ db: OpaquePointer, record: ViewActionIntentStore.Record) throws {
        guard let existing: ViewActionIntentStore.Record = try get(db, table: "pending", id: record.request.commandId),
              existing.body == record.body else { return }
        if let ticket = Ticket(rawValue: record.request.commandId) {
            var state = try metadata(db)
            if ticket.generation == state.generation {
                state.retired = Self.merged(state.retired + [Interval(first: ticket.sequence, last: ticket.sequence)])
                guard state.retired.count <= ViewActionIntentStore.maxPending + 1 else { throw ViewActionIntentError.unreadableStore }
                try writeMetadata(db, state)
            }
        }
        try remove(db, table: "pending", id: record.request.commandId)
    }

    func markAcknowledged(_ db: OpaquePointer, acknowledgment: ActionJournalAcknowledgment) throws {
        var state = try metadata(db)
        guard acknowledgment.generation > 0 else { throw ViewActionIntentError.invalidRecord }
        if acknowledgment.generation < state.generation { return }
        guard acknowledgment.generation == state.generation, acknowledgment.dataEpoch == state.activeEpoch else { throw ViewActionIntentError.invalidRecord }
        let ranges = acknowledgment.ranges.map { Interval(first: $0.first, last: $0.last) }
        guard ranges.count <= ViewActionIntentStore.maxPending + 1, ranges.allSatisfy({ range in
            range.first > 0 && range.first <= range.last && state.retired.contains { $0.first <= range.first && range.last <= $0.last }
        }) else { throw ViewActionIntentError.invalidRecord }
        state.acknowledged = Self.merged(state.acknowledged + ranges)
        guard state.acknowledged.count <= ViewActionIntentStore.maxPending + 1 else { throw ViewActionIntentError.invalidRecord }
        try writeMetadata(db, state)
    }
    private static func count(_ ranges: [Interval]) -> UInt64 {
        ranges.reduce(0) { $0 + $1.last - $1.first + 1 }
    }
    private static func merged(_ ranges: [Interval]) -> [Interval] {
        var result: [Interval] = []
        for range in ranges.sorted(by: { $0.first < $1.first }) {
            if let last = result.last, range.first <= last.last + 1 { result[result.count - 1].last = max(last.last, range.last) }
            else { result.append(range) }
        }
        return result
    }
    func acknowledgments(_ db: OpaquePointer) throws -> ViewActionIntentStore.Acknowledgments {
        let state = try metadata(db)
        return .init(generation: state.generation, dataEpoch: state.activeEpoch, ranges: state.retired)
    }
    func statistics(_ db: OpaquePointer) throws -> ViewActionIntentStore.Statistics {
        let state = try metadata(db)
        let bytes = try Self.integer(db, "SELECT (SELECT coalesce(sum(length(body)),0) FROM pending)+(SELECT coalesce(sum(length(body)),0) FROM legacy)+(SELECT coalesce(sum(length(body)),0) FROM recent)+(SELECT length(body) FROM metadata)")
        let fileBytes = try Self.integer(db, "PRAGMA page_count") * Self.integer(db, "PRAGMA page_size")
        return .init(pending: try Self.integer(db, "SELECT count(*) FROM pending"), recent: try Self.integer(db, "SELECT count(*) FROM recent"),
            legacy: try Self.integer(db, "SELECT count(*) FROM legacy"), retiredIntervals: state.retired.count, encryptedBytes: bytes, databaseBytes: fileBytes)
    }
    private func proofBytes(_ record: ViewActionIntentStore.Record) throws -> Int {
        let proof = ViewActionIntentStore.Completed(commandID: record.request.commandId,
            bodyDigest: Data(SHA256.hash(data: record.body)), dataEpoch: record.request.dataEpoch, response: nil)
        return try JSONEncoder().encode(proof).count + 28
    }
    private func requirePendingBudget(_ db: OpaquePointer) throws {
        guard try Self.integer(db, "SELECT coalesce(sum(length(body)),0) FROM pending") <= ViewActionIntentStore.maxBytes - ViewActionIntentStore.maxPending * 4 else { throw ViewActionIntentError.capacityExceeded }
    }
    private func trimRecent(_ db: OpaquePointer) throws {
        while try Self.integer(db, "SELECT count(*) FROM recent") > 64 || Self.integer(db, "SELECT coalesce(sum(length(body)),0) FROM recent") > 512 * 1024 {
            try Self.execute(db, "DELETE FROM recent WHERE rowid=(SELECT min(rowid) FROM recent)")
        }
    }
    private func metadata(_ db: OpaquePointer) throws -> Metadata {
        let statement = try Self.prepare(db, "SELECT body FROM metadata WHERE id=1"); defer { sqlite3_finalize(statement) }
        guard sqlite3_step(statement) == SQLITE_ROW else { throw ViewActionIntentError.unreadableStore }
        let value: Metadata = try decrypt(Self.bytes(statement), context: Data("metadata".utf8))
        guard value.floor > 0, value.floor <= value.next, value.next <= Self.maxCounter + 1, value.generation <= Self.maxCounter,
              (value.generation == 0 ? value.activeEpoch.isEmpty : !value.activeEpoch.isEmpty), value.retired.count <= ViewActionIntentStore.maxPending + 1,
              value.retired.allSatisfy({ $0.first > 0 && $0.first <= $0.last && $0.last < value.next }),
              zip(value.retired, value.retired.dropFirst()).allSatisfy({ pair in pair.0.last + 1 < pair.1.first }),
              value.acknowledged.count <= ViewActionIntentStore.maxPending + 1,
              value.acknowledged.allSatisfy({ range in range.first > 0 && range.first <= range.last
                && value.retired.contains(where: { $0.first <= range.first && range.last <= $0.last }) }),
              zip(value.acknowledged, value.acknowledged.dropFirst()).allSatisfy({ pair in pair.0.last + 1 < pair.1.first }) else { throw ViewActionIntentError.unreadableStore }
        return value
    }
    private func writeMetadata(_ db: OpaquePointer, _ state: Metadata) throws {
        let statement = try Self.prepare(db, "INSERT INTO metadata(id,body) VALUES(1,?) ON CONFLICT(id) DO UPDATE SET body=excluded.body")
        defer { sqlite3_finalize(statement) }; try Self.bind(try encrypt(state, context: Data("metadata".utf8)), statement, 1)
        guard sqlite3_step(statement) == SQLITE_DONE else { throw ViewActionIntentError.unreadableStore }
    }
    private func identifier(_ id: String) -> Data { Data(HMAC<SHA256>.authenticationCode(for: Data(id.utf8), using: indexKey)) }
    private func get<T: Decodable>(_ db: OpaquePointer, table: String, id: String) throws -> T? {
        let statement = try Self.prepare(db, "SELECT body FROM \(table) WHERE id=?"); defer { sqlite3_finalize(statement) }
        let digest = identifier(id); try Self.bind(digest, statement, 1)
        switch sqlite3_step(statement) {
        case SQLITE_DONE: return nil
        case SQLITE_ROW:
            let value: T = try decrypt(Self.bytes(statement), context: Data(table.utf8) + digest)
            guard try rowID(value) == id else { throw ViewActionIntentError.unreadableStore }
            return value
        default: throw ViewActionIntentError.unreadableStore
        }
    }
    private func all<T: Decodable>(_ db: OpaquePointer, table: String) throws -> [T] {
        let statement = try Self.prepare(db, "SELECT body,id FROM \(table) ORDER BY rowid"); defer { sqlite3_finalize(statement) }
        var values: [T] = []
        while true {
            switch sqlite3_step(statement) {
            case SQLITE_DONE: return values
            case SQLITE_ROW:
                let digest = try Self.bytes(statement, column: 1)
                let value: T = try decrypt(Self.bytes(statement), context: Data(table.utf8) + digest)
                guard identifier(try rowID(value)) == digest else { throw ViewActionIntentError.unreadableStore }
                values.append(value)
            default: throw ViewActionIntentError.unreadableStore
            }
        }
    }
    private func rowID<T>(_ value: T) throws -> String {
        if let record = value as? ViewActionIntentStore.Record {
            try ViewActionIntentStore.validate(record)
            return record.request.commandId
        }
        if let completed = value as? ViewActionIntentStore.Completed {
            guard !completed.commandID.isEmpty, !completed.dataEpoch.isEmpty, completed.bodyDigest.count == 32 else { throw ViewActionIntentError.unreadableStore }
            return completed.commandID
        }
        throw ViewActionIntentError.unreadableStore
    }
    private func put<T: Encodable>(_ db: OpaquePointer, table: String, id: String, value: T) throws {
        let statement = try Self.prepare(db, "INSERT INTO \(table)(id,body) VALUES(?,?) ON CONFLICT(id) DO UPDATE SET body=excluded.body")
        defer { sqlite3_finalize(statement) }; let digest = identifier(id)
        try Self.bind(digest, statement, 1); try Self.bind(try encrypt(value, context: Data(table.utf8) + digest), statement, 2)
        guard sqlite3_step(statement) == SQLITE_DONE else { throw ViewActionIntentError.unreadableStore }
    }
    private func remove(_ db: OpaquePointer, table: String, id: String) throws {
        let statement = try Self.prepare(db, "DELETE FROM \(table) WHERE id=?"); defer { sqlite3_finalize(statement) }
        try Self.bind(identifier(id), statement, 1)
        guard sqlite3_step(statement) == SQLITE_DONE else { throw ViewActionIntentError.unreadableStore }
    }
    private func encrypt<T: Encodable>(_ value: T, context: Data) throws -> Data {
        let sealed = try AES.GCM.seal(JSONEncoder().encode(value), using: key, authenticating: binding + context)
        guard let bytes = sealed.combined else { throw ViewActionIntentError.unreadableStore }; return bytes
    }
    private func decrypt<T: Decodable>(_ bytes: Data, context: Data) throws -> T {
        do { return try JSONDecoder().decode(T.self, from: AES.GCM.open(AES.GCM.SealedBox(combined: bytes), using: key, authenticating: binding + context)) }
        catch { throw ViewActionIntentError.unreadableStore }
    }
    private static func bind(_ data: Data, _ statement: OpaquePointer, _ index: Int32) throws {
        let result = data.withUnsafeBytes { sqlite3_bind_blob(statement, index, $0.baseAddress, Int32($0.count), transient) }
        guard result == SQLITE_OK else { throw ViewActionIntentError.unreadableStore }
    }
    private static func bytes(_ statement: OpaquePointer, column: Int32 = 0) throws -> Data {
        let count = sqlite3_column_bytes(statement, column)
        guard count > 0, count <= ViewActionIntentStore.maxBytes + 64, let pointer = sqlite3_column_blob(statement, column) else { throw ViewActionIntentError.unreadableStore }
        return Data(bytes: pointer, count: Int(count))
    }
    private static func integer(_ db: OpaquePointer, _ sql: String) throws -> Int {
        let statement = try prepare(db, sql); defer { sqlite3_finalize(statement) }
        guard sqlite3_step(statement) == SQLITE_ROW else { throw ViewActionIntentError.unreadableStore }
        return Int(sqlite3_column_int64(statement, 0))
    }
    private static func prepare(_ db: OpaquePointer, _ sql: String) throws -> OpaquePointer {
        var pointer: OpaquePointer?
        guard sqlite3_prepare_v2(db, sql, -1, &pointer, nil) == SQLITE_OK, let pointer else { throw ViewActionIntentError.unreadableStore }
        return pointer
    }
    private static func execute(_ db: OpaquePointer, _ sql: String) throws {
        guard sqlite3_exec(db, sql, nil, nil, nil) == SQLITE_OK else { throw ViewActionIntentError.unreadableStore }
    }
}
