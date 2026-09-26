import Darwin
import Foundation

@_silgen_name("flock")
private func feedbackFileLock(_ descriptor: Int32, _ operation: Int32) -> Int32

/// Status from the durable encrypted outbox. Absence must be proven, never inferred
/// from HTTP 404 or a timeout. A completed non-2xx response is a definite rejection.
public enum ReportFeedbackStatus: Sendable {
    case notDispatched, confirmed, rejected(Int), unknown
}

public struct ReportFeedbackRecoveryEvent: Sendable, Equatable {
    public enum Outcome: Sendable, Equatable {
        case confirmed, messageRejected(Int), readRejected(Int), awaitingDraft, unknown
    }
    public let reportID: UInt64
    public let operationID: UUID
    public let scope: CommandScope
    /// Keyed semantic binding, never the draft itself. Nil means no checkpoint was saved.
    public let binding: Data?
    public let outcome: Outcome
}

public actor ReportFeedbackTransaction {
    private enum Stage: String, Codable {
        case prepared, messageSentAwaitingRead, readStarted, complete, messageRejected, readRejected
    }
    private struct Checkpoint: Codable {
        let operationID: UUID
        let scope: CommandScope
        let reportID: UInt64
        let binding: Data
        var stage: Stage
        var readAttempt = 0
        var rejectionStatus: Int?
        var messageID: String { operationID.uuidString + "-message" }
        var readID: String { operationID.uuidString + "-read" + (readAttempt == 0 ? "" : "-\(readAttempt)") }
    }
    private let directory: URL
    private let gate: CommandGate
    private let scope: CommandScope
    private var running: Set<UInt64> = []
    private var recovered: [UInt64: ReportFeedbackRecoveryEvent] = [:]
    private var observers: [UUID: AsyncStream<[ReportFeedbackRecoveryEvent]>.Continuation] = [:]

    public init(directory: URL, gate: CommandGate, scope: CommandScope) {
        self.directory = directory; self.gate = gate; self.scope = scope
    }

    /// Replays the latest recovery snapshot to newly opened scenes. Recovery never carries
    /// an executable message or asks a view to reconstruct a previously submitted draft.
    public func recoveryEvents() -> AsyncStream<[ReportFeedbackRecoveryEvent]> {
        let id = UUID()
        return AsyncStream(bufferingPolicy: .bufferingNewest(1)) { continuation in
            observers[id] = continuation
            continuation.yield(recoverySnapshot)
            continuation.onTermination = { _ in Task { await self.removeObserver(id) } }
        }
    }

    /// Restore headlessly. The only mutation permitted here is acknowledging a report
    /// whose feedback message is already confirmed. Unknown messages are status-only.
    public func recover(retryingReadFor operationID: UUID? = nil,
        markRead: @escaping @Sendable (UInt64, String) async throws -> Void,
        reconcile: @escaping @Sendable (String) async throws -> ReportFeedbackStatus
    ) async throws -> [ReportFeedbackRecoveryEvent] {
        try await ensureActive()
        let reservations = try await gate.restore().filter {
            if case .replyToReport = $0.kind { return true }; return false
        }
        let files: [URL]
        do { files = try FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil) }
        catch let error as CocoaError where error.code == .fileReadNoSuchFile { files = [] }
        var ids = Set(files.filter { $0.pathExtension == "json" }.compactMap { UInt64($0.deletingPathExtension().lastPathComponent) })
        for command in reservations { if case .replyToReport(let id) = command.kind { ids.insert(id) } }
        for reportID in ids.sorted() {
            try await ensureActive()
            guard !running.contains(reportID) else { continue }
            running.insert(reportID)
            defer { running.remove(reportID) }
            let lease: Int32
            do { lease = try acquireLease(reportID) }
            catch FeedbackError.busy { continue }
            defer { _ = feedbackFileLock(lease, LOCK_UN); _ = Darwin.close(lease) }
            guard var checkpoint = try load(reportID) else {
                guard let command = reservations.first(where: { $0.kind == .replyToReport(reportID) }) else { continue }
                guard operationID == nil || operationID == command.id else { continue }
                let status = try await reconcile(command.id.uuidString + "-message")
                let absent: Bool
                if case .notDispatched = status { absent = await gate.confirmReconciled(command.id, in: scope) }
                else { absent = false }
                publish(.init(reportID: reportID, operationID: command.id, scope: scope,
                    binding: nil, outcome: absent ? .awaitingDraft : .unknown))
                continue
            }
            guard checkpoint.scope.sameIdentity(as: scope),
                  operationID == nil || operationID == checkpoint.operationID else { continue }
            if checkpoint.stage == .prepared {
                let status = try await reconcile(checkpoint.messageID)
                switch status {
                case .notDispatched:
                    // No payload is available, and recovery never invents one. Free the
                    // pre-dispatch reservation so an explicit new draft can be submitted.
                    guard await gate.confirmReconciled(checkpoint.operationID, in: scope) else { continue }
                    try FileManager.default.removeItem(at: directory.appendingPathComponent("\(reportID).json"))
                    publish(event(checkpoint, .awaitingDraft))
                    continue
                case .confirmed: checkpoint.stage = .messageSentAwaitingRead; try save(checkpoint)
                case .rejected(let status): checkpoint.stage = .messageRejected; checkpoint.rejectionStatus = status; try save(checkpoint)
                case .unknown: publish(event(checkpoint, .unknown)); continue
                }
            }
            if checkpoint.stage == .readRejected, operationID == checkpoint.operationID {
                checkpoint.readAttempt += 1
                checkpoint.stage = .messageSentAwaitingRead
                checkpoint.rejectionStatus = nil
                try save(checkpoint)
            }
            // Prepared was handled above, so this closure cannot be called. Keeping
            // message dispatch absent from the public recovery API is the safety boundary.
            try await drive(checkpoint, sendMessage: { _ in throw FeedbackError.unresolved },
                            markRead: { id in try await markRead(reportID, id) }, reconcile: reconcile)
            guard let latest = try load(reportID) else { throw FeedbackError.unresolved }
            let result = await outcome(reportID)
            switch result {
            case .confirmed: publish(event(latest, .confirmed))
            case .rejected:
                if latest.stage == .messageRejected { publish(event(latest, .messageRejected(latest.rejectionStatus ?? 0))) }
                else if latest.stage == .readRejected { publish(event(latest, .readRejected(latest.rejectionStatus ?? 0))) }
                else { publish(event(latest, .unknown)) }
            case .unknown, .obsolete: publish(event(latest, .unknown))
            }
        }
        return recoverySnapshot
    }

    private var recoverySnapshot: [ReportFeedbackRecoveryEvent] { recovered.values.sorted { $0.reportID < $1.reportID } }
    private func removeObserver(_ id: UUID) { observers[id] = nil }
    private func publish(_ event: ReportFeedbackRecoveryEvent) {
        recovered[event.reportID] = event
        for observer in observers.values { observer.yield(recoverySnapshot) }
    }
    private func event(_ checkpoint: Checkpoint, _ outcome: ReportFeedbackRecoveryEvent.Outcome) -> ReportFeedbackRecoveryEvent {
        .init(reportID: checkpoint.reportID, operationID: checkpoint.operationID,
              scope: scope, binding: checkpoint.binding, outcome: outcome)
    }

    public func submit(reportID: UInt64, binding: Data,
        sendMessage: @escaping @Sendable (String) async throws -> Void,
        markRead: @escaping @Sendable (String) async throws -> Void,
        reconcile: @escaping @Sendable (String) async throws -> ReportFeedbackStatus
    ) async -> CommandResult {
        guard await gate.scope == scope else { return .obsolete }
        guard !running.contains(reportID) else { return .rejected(FeedbackError.busy) }
        running.insert(reportID)
        defer { running.remove(reportID) }
        do {
            let lease = try acquireLease(reportID)
            defer { _ = feedbackFileLock(lease, LOCK_UN); _ = Darwin.close(lease) }
            var checkpoint = try load(reportID)
            if let previous = checkpoint, previous.stage == .messageRejected {
                guard await gate.confirmReconciled(previous.operationID, in: scope) else { return unknown(previous) }
                // This message definitively did not execute. A new explicit submit may
                // reserve a fresh operation with the user's current draft.
                try FileManager.default.removeItem(at: directory.appendingPathComponent("\(reportID).json"))
                checkpoint = nil
            }
            if checkpoint == nil,
               let reserved = try await gate.restore().first(where: {
                   $0.kind == .replyToReport(reportID) && $0.itemID == "r\(reportID)"
               }) {
                // Crash after gate reservation, before checkpoint. Only proven absence
                // permits attaching the current draft; an existing envelope has no binding.
                guard case .notDispatched = try await reconcile(reserved.id.uuidString + "-message") else {
                    return .unknown(reserved)
                }
                checkpoint = Checkpoint(operationID: reserved.id, scope: scope,
                    reportID: reportID, binding: binding, stage: .prepared)
                try save(checkpoint!)
            }
            if var existing = checkpoint {
                guard existing.scope.sameIdentity(as: scope), existing.reportID == reportID else {
                    return .rejected(FeedbackError.wrongIdentity)
                }
                guard existing.binding == binding else { return .rejected(FeedbackError.differentDraft) }
                if existing.stage == .readRejected {
                    existing.readAttempt += 1
                    existing.stage = .messageSentAwaitingRead
                    existing.rejectionStatus = nil
                    try save(existing)
                }
                try await drive(existing, sendMessage: sendMessage, markRead: markRead, reconcile: reconcile)
                return await outcome(reportID)
            }
            let result = await gate.submitIdentified(.replyToReport(reportID), itemID: "r\(reportID)",
                in: scope, isUnknown: { _ in true }) { id in
                    let checkpoint = Checkpoint(operationID: id, scope: self.scope,
                        reportID: reportID, binding: binding, stage: .prepared)
                    try await self.save(checkpoint)
                    try await self.drive(checkpoint, sendMessage: sendMessage, markRead: markRead, reconcile: reconcile)
                    guard case .confirmed = await self.currentStatus(reportID) else { throw FeedbackError.unresolved }
                }
            if case .unknown = result { return await outcome(reportID) }
            return result
        } catch {
            // Existing metadata always wins over a transport or persistence exception.
            if let checkpoint = try? load(reportID) { return unknown(checkpoint) }
            return .rejected(error)
        }
    }

    private func drive(_ initial: Checkpoint,
        sendMessage: @Sendable (String) async throws -> Void,
        markRead: @Sendable (String) async throws -> Void,
        reconcile: @Sendable (String) async throws -> ReportFeedbackStatus
    ) async throws {
        var checkpoint = initial
        if checkpoint.stage == .prepared {
            try await ensureActive()
            var status = try await reconcile(checkpoint.messageID)
            if case .notDispatched = status {
                do { try await sendMessage(checkpoint.messageID); status = .confirmed }
                catch { status = try await reconcile(checkpoint.messageID) }
            }
            switch status {
            case .confirmed: checkpoint.stage = .messageSentAwaitingRead
            case .rejected(let code): checkpoint.stage = .messageRejected; checkpoint.rejectionStatus = code
            case .notDispatched, .unknown: return
            }
            try save(checkpoint)
        }
        if checkpoint.stage == .messageSentAwaitingRead || checkpoint.stage == .readStarted {
            checkpoint.stage = .readStarted
            try save(checkpoint)
            try await ensureActive()
            var status = try await reconcile(checkpoint.readID)
            if case .notDispatched = status {
                do { try await markRead(checkpoint.readID); status = .confirmed }
                catch { status = try await reconcile(checkpoint.readID) }
            }
            switch status {
            case .confirmed: checkpoint.stage = .complete
            case .rejected(let code): checkpoint.stage = .readRejected; checkpoint.rejectionStatus = code
            case .notDispatched, .unknown: return
            }
            try save(checkpoint)
        }
    }

    private func currentStatus(_ report: UInt64) -> ReportFeedbackStatus {
        guard let checkpoint = try? load(report) else { return .unknown }
        switch checkpoint.stage {
        case .complete: return .confirmed
        case .messageRejected, .readRejected: return .rejected(checkpoint.rejectionStatus ?? 0)
        default: return .unknown
        }
    }

    private func outcome(_ report: UInt64) async -> CommandResult {
        guard let checkpoint = try? load(report) else { return .rejected(FeedbackError.unresolved) }
        switch checkpoint.stage {
        case .complete, .messageRejected:
            guard await gate.confirmReconciled(checkpoint.operationID, in: scope) else { return unknown(checkpoint) }
            return checkpoint.stage == .complete ? .confirmed : .rejected(FeedbackError.http(checkpoint.rejectionStatus ?? 0))
        case .readRejected: return .rejected(FeedbackError.http(checkpoint.rejectionStatus ?? 0))
        default: return unknown(checkpoint)
        }
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
    private enum FeedbackError: LocalizedError {
        case busy, wrongIdentity, differentDraft, unresolved, http(Int)
        var errorDescription: String? {
            switch self {
            case .busy: return "Feedback for this report is already being processed."
            case .wrongIdentity: return "Reconnect the device before submitting feedback."
            case .differentDraft: return "This report has feedback from another draft. Your current draft has not been sent."
            case .unresolved: return "The feedback result could not be verified."
            case .http(let status): return "The service rejected this operation (HTTP \(status)). Your draft has been kept."
            }
        }
    }
}
