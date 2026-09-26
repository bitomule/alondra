import AlondraCommands
import Foundation
import Darwin

/// Presentation metadata only. No request payload, command reservation or executable retry.
public struct DeliveryWarningRecord: Codable, Sendable {
    public let id: UUID
    public let scope: CommandScope
    public let firstObservedUncertainAt: Date
    public var acknowledgedPhase: DeliveryStatus.Phase?
}

/// Synchronous storage keeps metadata updates atomic with respect to this actor's state.
public struct DeliveryWarningStorage: Sendable {
    public var load: @Sendable () throws -> [DeliveryWarningRecord]
    public var save: @Sendable ([DeliveryWarningRecord]) throws -> Void
    public init(load: @escaping @Sendable () throws -> [DeliveryWarningRecord],
                save: @escaping @Sendable ([DeliveryWarningRecord]) throws -> Void) {
        self.load = load; self.save = save
    }
    public static func file(_ url: URL) -> Self {
        Self(load: {
            do { return try JSONDecoder().decode([DeliveryWarningRecord].self, from: Data(contentsOf: url)) }
            catch let error as CocoaError where error.code == .fileReadNoSuchFile { return [] }
        }, save: { records in
            try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true,
                                                    attributes: [.posixPermissions: 0o700])
            try JSONEncoder().encode(records).write(to: url, options: [.atomic, .completeFileProtectionUnlessOpen])
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
            let handle = try FileHandle(forWritingTo: url)
            defer { try? handle.close() }
            try handle.synchronize()
            let directory = Darwin.open(url.deletingLastPathComponent().path, O_RDONLY)
            guard directory >= 0 else { throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO) }
            defer { Darwin.close(directory) }
            guard Darwin.fsync(directory) == 0 else { throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO) }
        })
    }
}

public actor DeliveryStatusService {
    private let gate: CommandGate
    private let scope: CommandScope
    private let storage: DeliveryWarningStorage
    private let now: @Sendable () -> Date
    private let feedback: @Sendable () async -> AsyncStream<[ReportFeedbackRecoveryEvent]>
    private var records: [UUID: DeliveryWarningRecord] = [:]
    private var phases: [UUID: DeliveryStatus.Phase] = [:]
    private var commands: [UncertainCommand] = []
    private var feedbackEvents: [ReportFeedbackRecoveryEvent] = []
    private var loaded = false
    private var error: String?
    private var active = true
    private var observation: Task<Void, Never>?
    private var listeners: [UUID: AsyncStream<DeliveryStatus>.Continuation] = [:]

    public init(gate: CommandGate, scope: CommandScope, storage: DeliveryWarningStorage,
                now: @escaping @Sendable () -> Date = { Date() },
                feedback: @escaping @Sendable () async -> AsyncStream<[ReportFeedbackRecoveryEvent]> = { AsyncStream { $0.finish() } }) {
        self.gate = gate; self.scope = scope; self.storage = storage; self.now = now; self.feedback = feedback
    }

    public func observe() async -> AsyncStream<DeliveryStatus> {
        guard active else { return AsyncStream { $0.finish() } }
        _ = await refresh()
        guard active else { return AsyncStream { $0.finish() } }
        if observation == nil {
            observation = Task {
                await withTaskGroup(of: Void.self) { group in
                    group.addTask {
                        do {
                            for await commands in try await self.gate.observeUncertainty() {
                                guard !Task.isCancelled else { return }
                                await self.receive(commands)
                            }
                        } catch { await self.storageFailed(error) }
                    }
                    group.addTask {
                        for await events in await self.feedback() {
                            guard !Task.isCancelled else { return }
                            await self.receiveFeedback(events)
                        }
                    }
                    await group.waitForAll()
                }
            }
        }
        let id = UUID()
        return AsyncStream(bufferingPolicy: .bufferingNewest(1)) { continuation in
            listeners[id] = continuation
            continuation.yield(snapshot)
            continuation.onTermination = { _ in Task { await self.remove(id) } }
        }
    }

    /// Also makes relaunch restoration deterministic for callers before opening a scene.
    public func refresh() async -> DeliveryStatus {
        guard active else { return snapshot }
        do {
            let commands = try await gate.restore()
            guard active, await gate.scope == scope else { return snapshot }
            await receive(commands)
        } catch { storageFailed(error) }
        return snapshot
    }

    /// Hides only the phase the user saw. A newer outcome must remain visible even if
    /// its operation UUID is unchanged. The gate and encrypted outbox are untouched.
    public func acknowledge(_ observed: [UUID: DeliveryStatus.Phase], in expectedScope: CommandScope) async throws {
        guard active, expectedScope == scope, await gate.scope == scope else { throw Failure.obsolete }
        guard active else { throw Failure.obsolete }
        guard loaded else { throw Failure.storageUnavailable }
        var next = records
        var changed = false
        for (id, observedPhase) in observed {
            guard let phase = phases[id], phase == observedPhase else { continue }
            guard var record = next[id] else { throw Failure.storageUnavailable }
            guard record.acknowledgedPhase != phase else { continue }
            record.acknowledgedPhase = phase
            next[id] = record
            changed = true
        }
        guard changed else { publish(); return }
        do { try storage.save(Array(next.values)) }
        catch { storageFailed(error); throw error }
        records = next; error = nil; publish()
    }

    public func stop() {
        active = false
        observation?.cancel(); observation = nil
        for listener in listeners.values { listener.finish() }
        listeners.removeAll()
    }

    private func remove(_ id: UUID) {
        listeners[id] = nil
        if listeners.isEmpty { observation?.cancel(); observation = nil }
    }
    private func receive(_ commands: [UncertainCommand]) async {
        guard await gate.scope == scope else { stop(); return }
        guard active, !Task.isCancelled else { return }
        self.commands = commands.filter { $0.scope.sameIdentity(as: scope) }
        rebuild()
    }
    private func receiveFeedback(_ events: [ReportFeedbackRecoveryEvent]) async {
        guard await gate.scope == scope else { stop(); return }
        guard active, !Task.isCancelled else { return }
        feedbackEvents = events.filter { $0.scope.sameIdentity(as: scope) }
        rebuild()
    }
    private func storageFailed(_ failure: any Error) {
        guard active, !Task.isCancelled else { return }
        error = failure.localizedDescription
        publish()
    }
    private func rebuild() {
        phases = Dictionary(commands.map { ($0.id, DeliveryStatus.Phase.unconfirmed) }, uniquingKeysWith: { first, _ in first })
        for event in feedbackEvents {
            switch event.outcome {
            case .confirmed: phases[event.operationID] = nil
            case .unknown: phases[event.operationID] = .unconfirmed
            case .messageRejected(let code): phases[event.operationID] = .feedbackMessageRejected(code)
            case .readRejected(let code): phases[event.operationID] = .feedbackReadRejected(code)
            case .awaitingDraft: phases[event.operationID] = .feedbackNeedsDraft
            }
        }
        do {
            if !loaded {
                let saved = try storage.load()
                // A service owns one identity-specific file. Never overwrite a different identity's metadata.
                guard saved.allSatisfy({ $0.scope.sameIdentity(as: scope) }) else { throw Failure.obsolete }
                records = Dictionary(saved.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
                loaded = true
            }
            var next = records
            for id in phases.keys where next[id] == nil {
                next[id] = DeliveryWarningRecord(id: id, scope: scope, firstObservedUncertainAt: now())
            }
            if next.count != records.count { try storage.save(Array(next.values)) }
            records = next; error = nil
        } catch { self.error = error.localizedDescription }
        publish()
    }
    private var snapshot: DeliveryStatus {
        DeliveryStatus(scope: scope, operations: phases.map { id, phase in
            DeliveryStatus.Operation(id: id, phase: phase,
                firstObservedUncertainAt: records[id]?.firstObservedUncertainAt ?? now(),
                acknowledged: records[id]?.acknowledgedPhase == phase)
        }.sorted { $0.id.uuidString < $1.id.uuidString }, persistenceError: error)
    }
    private func publish() { for listener in listeners.values { listener.yield(snapshot) } }
    public enum Failure: Error { case obsolete, storageUnavailable }
}
