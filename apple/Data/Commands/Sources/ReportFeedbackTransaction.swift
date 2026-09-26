import Darwin
import Foundation

@_silgen_name("flock")
private func feedbackFileLock(_ descriptor: Int32, _ operation: Int32) -> Int32

/// A message and its read acknowledgement are one durable semantic operation.
/// Checkpoints contain identifiers only. Uncertain dispatch is reconciled by status;
/// neither a recreated actor nor a different process is allowed to resend a message.
public actor ReportFeedbackTransaction {
    private enum Stage: String, Codable { case prepared, messageSentAwaitingRead, readStarted, complete }
    private struct Checkpoint: Codable {
        let operationID: UUID
        let scope: CommandScope
        let reportID: UInt64
        var stage: Stage
    }
    private let directory: URL
    private let gate: CommandGate
    private let scope: CommandScope
    private var running: Set<UInt64> = []

    public init(directory: URL, gate: CommandGate, scope: CommandScope) {
        self.directory = directory; self.gate = gate; self.scope = scope
    }

    public func submit(reportID: UInt64,
        sendMessage: @escaping @Sendable (String) async throws -> Void,
        markRead: @escaping @Sendable (String) async throws -> Void,
        reconcile: @escaping @Sendable (String) async throws -> Bool
    ) async -> CommandResult {
        guard await gate.scope == scope else { return .obsolete }
        guard !running.contains(reportID) else { return .rejected(FeedbackError.busy) }
        running.insert(reportID)
        defer { running.remove(reportID) }
        do {
            let lease = try acquireLease(reportID)
            defer { _ = feedbackFileLock(lease, LOCK_UN); _ = Darwin.close(lease) }
            if var checkpoint = try load(reportID) {
                guard checkpoint.scope.sameIdentity(as: scope), checkpoint.reportID == reportID else {
                    return .rejected(FeedbackError.wrongIdentity)
                }
                do {
                    if checkpoint.stage == .prepared {
                        guard try await reconcile(checkpoint.operationID.uuidString + "-message") else {
                            return unknown(checkpoint)
                        }
                        checkpoint.stage = .messageSentAwaitingRead
                        try save(checkpoint)
                    }
                    if checkpoint.stage == .messageSentAwaitingRead {
                        checkpoint.stage = .readStarted
                        try save(checkpoint)
                        try await ensureActive()
                        try await markRead(checkpoint.operationID.uuidString + "-read")
                        checkpoint.stage = .complete
                        try save(checkpoint)
                    } else if checkpoint.stage == .readStarted {
                        // APIClient's durable transport reconciles an existing sub-ID without
                        // POST. If a crash preceded its outbox reservation, this starts only
                        // the read acknowledgement; the message remains confirmed.
                        try await ensureActive()
                        try await markRead(checkpoint.operationID.uuidString + "-read")
                        checkpoint.stage = .complete
                        try save(checkpoint)
                    }
                    guard await gate.confirmReconciled(checkpoint.operationID, in: scope) else {
                        return unknown(checkpoint)
                    }
                    return .confirmed
                } catch { return unknown(checkpoint) }
            }
            return await gate.submitIdentified(.replyToReport(reportID), itemID: "r\(reportID)",
                in: scope, isUnknown: { _ in true }) { id in
                    var checkpoint = Checkpoint(operationID: id, scope: self.scope,
                                                reportID: reportID, stage: .prepared)
                    try await self.save(checkpoint)
                    try await self.ensureActive()
                    try await sendMessage(id.uuidString + "-message")
                    checkpoint.stage = .messageSentAwaitingRead
                    try await self.save(checkpoint)
                    checkpoint.stage = .readStarted
                    try await self.save(checkpoint)
                    try await self.ensureActive()
                    try await markRead(id.uuidString + "-read")
                    checkpoint.stage = .complete
                    try await self.save(checkpoint)
                }
        } catch { return .rejected(error) }
    }

    private func ensureActive() async throws {
        try Task.checkCancellation()
        guard await gate.scope == scope else { throw CancellationError() }
    }

    private func unknown(_ checkpoint: Checkpoint) -> CommandResult {
        .unknown(UncertainCommand(id: checkpoint.operationID, scope: scope,
            kind: .replyToReport(checkpoint.reportID), itemID: "r\(checkpoint.reportID)"))
    }

    private func acquireLease(_ report: UInt64) throws -> Int32 {
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true,
                                                attributes: [.posixPermissions: 0o700])
        let descriptor = Darwin.open(directory.appendingPathComponent("\(report).lock").path,
                                     O_RDWR | O_CREAT, 0o600)
        guard descriptor >= 0 else { throw posixError() }
        guard feedbackFileLock(descriptor, LOCK_EX | LOCK_NB) == 0 else {
            _ = Darwin.close(descriptor)
            throw FeedbackError.busy
        }
        return descriptor
    }

    private func load(_ report: UInt64) throws -> Checkpoint? {
        let file = directory.appendingPathComponent("\(report).json")
        do { return try JSONDecoder().decode(Checkpoint.self, from: Data(contentsOf: file)) }
        catch let error as CocoaError where error.code == .fileReadNoSuchFile { return nil }
    }

    private func save(_ checkpoint: Checkpoint) throws {
        let data = try JSONEncoder().encode(checkpoint)
        let temporary = directory.appendingPathComponent(".\(UUID().uuidString).tmp")
        let descriptor = Darwin.open(temporary.path, O_WRONLY | O_CREAT | O_EXCL, 0o600)
        guard descriptor >= 0 else { throw posixError() }
        defer { _ = Darwin.close(descriptor); _ = Darwin.unlink(temporary.path) }
        try data.withUnsafeBytes { bytes in
            var offset = 0
            while offset < bytes.count {
                let count = Darwin.write(descriptor, bytes.baseAddress!.advanced(by: offset), bytes.count - offset)
                if count < 0 && errno == EINTR { continue }
                guard count > 0 else { throw posixError() }
                offset += count
            }
        }
        guard Darwin.fsync(descriptor) == 0 else { throw posixError() }
        guard Darwin.rename(temporary.path, directory.appendingPathComponent("\(checkpoint.reportID).json").path) == 0
        else { throw posixError() }
        let parent = Darwin.open(directory.path, O_RDONLY)
        guard parent >= 0 else { throw posixError() }
        defer { _ = Darwin.close(parent) }
        guard Darwin.fsync(parent) == 0 else { throw posixError() }
    }

    private func posixError() -> POSIXError { POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO) }
    private enum FeedbackError: Error { case busy, wrongIdentity }
}
