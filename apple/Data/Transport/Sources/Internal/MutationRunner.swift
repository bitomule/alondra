import Foundation
import Synchronization

enum RequestInterruption: Error, Sendable, Equatable {
    case expired, cancelled
}

/// A reservation is conservative uncertainty for other callers sharing the same operation ID.
/// It never proves that a POST happened; only the dispatch boundary records that evidence.
final class MutationDispatchEvidence: Sendable {
    private struct State {
        var reservedOwners: Set<UUID> = []
        var possibleDispatch = false
    }
    private let state = Mutex(State())
    private let didRelease: @Sendable (MutationDispatchEvidence) -> Void

    init(didRelease: @escaping @Sendable (MutationDispatchEvidence) -> Void = { _ in }) { self.didRelease = didRelease }
    var hasOwners: Bool { state.withLock { !$0.reservedOwners.isEmpty } }

    func reserve(by owner: UUID) { state.withLock { _ = $0.reservedOwners.insert(owner) } }
    func release(by owner: UUID) {
        state.withLock { _ = $0.reservedOwners.remove(owner) }
        didRelease(self)
    }
    func markPossibleDispatch() { state.withLock { $0.possibleDispatch = true } }
    func couldHaveEffect(for owner: UUID) -> Bool {
        state.withLock { $0.possibleDispatch || $0.reservedOwners.count > ($0.reservedOwners.contains(owner) ? 1 : 0) }
    }
}

final class MutationBudget: Sendable {
    private struct State {
        var interruption: RequestInterruption?
        var possibleDispatch: Bool
        var evidence: [MutationDispatchEvidence] = []
        var conservativeUnknown = false
    }
    let id: UUID
    let clock: RequestClock
    let deadline: ContinuousClock.Instant
    private let state: Mutex<State>

    init(clock: RequestClock, duration: Duration, knownDispatched: Bool, dispatchEvidence: MutationDispatchEvidence? = nil, conservativeUnknown: Bool = false,
         ownerID: UUID? = nil, startedAt: ContinuousClock.Instant? = nil) {
        id = ownerID ?? UUID()
        self.clock = clock
        deadline = (startedAt ?? clock.now()).advanced(by: duration)
        state = Mutex(State(possibleDispatch: knownDispatched, evidence: dispatchEvidence.map { [$0] } ?? [], conservativeUnknown: conservativeUnknown))
        dispatchEvidence?.reserve(by: id)
        if knownDispatched { dispatchEvidence?.markPossibleDispatch() }
    }

    var possibleDispatch: Bool {
        state.withLock { $0.possibleDispatch || $0.evidence.contains { $0.couldHaveEffect(for: id) } }
    }
    func track(_ evidence: MutationDispatchEvidence) throws {
        try state.withLock { state in
            guard !state.evidence.contains(where: { $0 === evidence }) else { return }
            guard state.evidence.count < 4 else { throw SealedRequestError.invalidRequest }
            if state.possibleDispatch { evidence.markPossibleDispatch() }
            state.evidence.append(evidence)
            evidence.reserve(by: id)
        }
    }
    var interruption: RequestInterruption? {
        state.withLock { state in
            if state.interruption == nil, clock.now() >= deadline { state.interruption = .expired }
            return state.interruption
        }
    }

    func stop(_ reason: RequestInterruption) { state.withLock { if $0.interruption == nil { $0.interruption = reason } } }
    func check() throws {
        if Task.isCancelled { stop(.cancelled) }
        if let interruption { throw interruption }
    }
    func noteExistingDispatch() {
        state.withLock { state in
            state.possibleDispatch = true
            for evidence in state.evidence { evidence.markPossibleDispatch() }
        }
    }
    var conservativeUnknown: Bool { state.withLock { $0.conservativeUnknown } }
    func noteOpaqueDelegation() { state.withLock { $0.conservativeUnknown = true } }
    func finishPhysicalWork() {
        state.withLock { state in
            for evidence in state.evidence { evidence.release(by: id) }
        }
    }
    func remaining(cap: Duration) throws -> Duration {
        try check()
        let remaining = min(cap, clock.now().duration(to: deadline))
        guard remaining > .zero else { stop(.expired); throw RequestInterruption.expired }
        return remaining
    }
    func markPossibleDispatch() throws {
        if Task.isCancelled { stop(.cancelled) }
        try state.withLock { state in
            if state.interruption == nil, clock.now() >= deadline { state.interruption = .expired }
            if let interruption = state.interruption { throw interruption }
            state.possibleDispatch = true
            for evidence in state.evidence { evidence.markPossibleDispatch() }
        }
    }
}

enum MutationScope {
    @TaskLocal static var current: MutationBudget?
    static func check() throws { try current?.check() }
    static func remaining(cap: Duration) throws -> Duration { try current?.remaining(cap: cap) ?? cap }
    static func possibleDispatch() throws { try current?.markPossibleDispatch() }
    static func reserve(_ evidence: MutationDispatchEvidence) throws {
        guard let budget = current else { return }
        try budget.track(evidence)
        try budget.check()
        evidence.reserve(by: budget.id)
    }
}

struct MutationRunnerStatus: Sendable {
    let workers: Int
    let waiters: Int
    let timers: Int
    let completions: Int
}

/// Logical callers may finish at their deadline while the four owned physical workers drain.
/// A worker's slot survives cancellation until its transport or shared wait actually returns.
actor MutationRunner {
    private enum Completion: Sendable {
        case worker(UUID, Task<Void, Never>)
        case timer(Task<Void, Never>)
    }
    private struct Call {
        let budget: MutationBudget
        let launch: @Sendable () -> Task<Void, Never>
        let interrupt: @Sendable (RequestInterruption) -> Void
        var awaitingResult = true
    }
    private let clock: RequestClock
    private let duration: Duration
    private var calls: [UUID: Call] = [:]
    private var pending: [UUID] = []
    private(set) var runningWorkers: [UUID: Task<Void, Never>] = [:]
    private(set) var timerTask: Task<Void, Never>?
    private var timerDeadline: ContinuousClock.Instant?
    private var retiringTimer = false
    private(set) var completionTask: Task<Void, Never>?
    private var retiringWorkers: Set<UUID> = []
    private var retiringBudgets: [UUID: MutationBudget] = [:]
    var waitingCallCount: Int { calls.values.filter(\.awaitingResult).count }
    private static let workerLimit = 4
    nonisolated let states: AsyncStream<MutationRunnerStatus>
    private let stateContinuation: AsyncStream<MutationRunnerStatus>.Continuation

    init(clock: RequestClock, duration: Duration = .seconds(120)) {
        self.clock = clock; self.duration = duration
        let pair = AsyncStream<MutationRunnerStatus>.makeStream(bufferingPolicy: .bufferingNewest(1))
        states = pair.stream; stateContinuation = pair.continuation
    }

    private func reportState() {
        stateContinuation.yield(MutationRunnerStatus(workers: runningWorkers.count, waiters: waitingCallCount, timers: timerTask == nil ? 0 : 1, completions: completionTask == nil ? 0 : 1))
    }

    func drain() async {
        while !runningWorkers.isEmpty || timerTask != nil || completionTask != nil {
            let workers = Array(runningWorkers.values)
            let timer = timerTask
            let completion = completionTask
            for worker in workers { await worker.value }
            await timer?.value
            await completion?.value
        }
    }

    func run<Output: Sendable>(knownDispatched: Bool = false, dispatchEvidence: MutationDispatchEvidence? = nil, conservativeUnknown: Bool = false,
                              ownerID: UUID? = nil, startedAt: ContinuousClock.Instant? = nil, unknown: @escaping @Sendable () -> Output,
                              operation: @escaping @Sendable () async throws -> Output) async throws -> Output {
        if let budget = MutationScope.current {
            if let dispatchEvidence {
                do { try budget.track(dispatchEvidence) }
                catch {
                    dispatchEvidence.release(by: budget.id)
                    throw error
                }
            }
            if knownDispatched { budget.noteExistingDispatch() }
            if conservativeUnknown { budget.noteOpaqueDelegation() }
            try budget.check()
            return try await operation()
        }
        let budget = MutationBudget(clock: clock, duration: duration, knownDispatched: knownDispatched, dispatchEvidence: dispatchEvidence, conservativeUnknown: conservativeUnknown, ownerID: ownerID, startedAt: startedAt)
        let id = budget.id
        return try await withTaskCancellationHandler {
            if Task.isCancelled { budget.stop(.cancelled) }
            return try await withCheckedThrowingContinuation { continuation in
                let interrupted: @Sendable (RequestInterruption) -> Void = { reason in
                    if budget.possibleDispatch || budget.conservativeUnknown { continuation.resume(returning: unknown()) }
                    else { continuation.resume(throwing: reason == .cancelled ? CancellationError() : URLError(.timedOut)) }
                }
                let launch: @Sendable () -> Task<Void, Never> = { [self] in
                    Task { @concurrent [self] in
                        let result: Result<Output, any Error>
                        do {
                            result = .success(try await MutationScope.$current.withValue(budget) {
                                try budget.check()
                                return try await operation()
                            })
                        } catch { result = .failure(error) }
                        await self.finished(id, result: result, continuation: continuation)
                    }
                }
                calls[id] = Call(budget: budget, launch: launch, interrupt: interrupted)
                pending.append(id)
                if let reason = budget.interruption { interrupt(id, reason: reason) }
                admit()
                armTimer()
                reportState()
            }
        } onCancel: {
            budget.stop(.cancelled)
            Task { await self.interrupt(id, reason: .cancelled) }
        }
    }

    private func admit() {
        while runningWorkers.count < Self.workerLimit, !pending.isEmpty {
            let id = pending.removeFirst()
            guard let call = calls[id], call.awaitingResult else { continue }
            if let reason = call.budget.interruption { interrupt(id, reason: reason); continue }
            runningWorkers[id] = call.launch()
        }
    }

    private func finished<Output: Sendable>(_ id: UUID, result: Result<Output, any Error>,
                                          continuation: CheckedContinuation<Output, any Error>) {
        guard let call = calls.removeValue(forKey: id) else { return }
        retiringWorkers.insert(id)
        retiringBudgets[id] = call.budget
        if case let .failure(error) = result, error is CancellationError { call.budget.stop(.cancelled) }
        if call.awaitingResult {
            if let reason = call.budget.interruption { call.interrupt(reason) }
            else { continuation.resume(with: result) }
        }
        watchCompletions()
        armTimer()
        reportState()
    }

    private func nextCompletion() -> Completion? {
        if retiringTimer, let timerTask { return .timer(timerTask) }
        guard let id = retiringWorkers.first, let worker = runningWorkers[id] else { return nil }
        return .worker(id, worker)
    }

    private func workerCompleted(_ id: UUID) {
        retiringWorkers.remove(id)
        retiringBudgets.removeValue(forKey: id)?.finishPhysicalWork()
        runningWorkers.removeValue(forKey: id)
        admit()
        armTimer()
        reportState()
    }

    private func watchCompletions() {
        guard completionTask == nil, !retiringWorkers.isEmpty || retiringTimer else { return }
        completionTask = Task { @concurrent [self] in
            while let completion = await self.nextCompletion() {
                switch completion {
                case let .worker(id, worker):
                    await worker.value
                    await self.workerCompleted(id)
                case let .timer(timer):
                    await timer.value
                    await self.timerCompleted()
                }
            }
            await self.completionsFinished()
        }
    }

    private func completionsFinished() {
        completionTask = nil
        watchCompletions()
        reportState()
    }

    func cancelAll() {
        for id in Array(calls.keys) { interrupt(id, reason: .cancelled) }
    }

    private func interrupt(_ id: UUID, reason: RequestInterruption) {
        guard var call = calls[id], call.awaitingResult else { return }
        call.budget.stop(reason)
        call.awaitingResult = false
        calls[id] = call
        call.interrupt(reason)
        pending.removeAll { $0 == id }
        if let worker = runningWorkers[id] { worker.cancel() }
        else { calls.removeValue(forKey: id)?.budget.finishPhysicalWork() }
        armTimer()
        reportState()
    }

    private func armTimer() {
        let waiting = calls.values.filter(\.awaitingResult)
        guard let deadline = waiting.map({ $0.budget.deadline }).min() else { timerTask?.cancel(); return }
        if let timerTask {
            if timerDeadline != deadline { timerTask.cancel() }
            return
        }
        let clock = clock
        timerDeadline = deadline
        timerTask = Task { @concurrent [self] in
            do { try await clock.waitUntil(deadline) } catch {}
            await self.timerReady()
        }
    }

    private func timerReady() {
        retiringTimer = true
        watchCompletions()
        reportState()
    }

    private func timerCompleted() {
        timerTask = nil
        timerDeadline = nil
        retiringTimer = false
        for id in Array(calls.keys) {
            guard let call = calls[id], call.awaitingResult, let reason = call.budget.interruption else { continue }
            interrupt(id, reason: reason)
        }
        armTimer()
        reportState()
    }
}

extension Duration {
    var requestTimeInterval: TimeInterval {
        let value = components
        return Double(value.seconds) + Double(value.attoseconds) / 1_000_000_000_000_000_000
    }
}

struct MutationEvidenceLease: Sendable {
    let requestID: String
    let evidence: MutationDispatchEvidence
    let ownerID: UUID
    fileprivate let referenceID: UUID
}

/// Carries evidence only while a call is live or its carrier retains an operation record.
final class MutationEvidenceStore: Sendable {
    private struct Entry {
        let evidence: MutationDispatchEvidence
        var references: Set<UUID> = []
        var retained = false
    }
    private let entries = Mutex<[String: Entry]>([:])
    var entryCount: Int { entries.withLock { $0.count } }

    func acquire(_ requestID: String, owner: UUID) -> MutationEvidenceLease {
        entries.withLock { entries in
            var entry = entries[requestID] ?? Entry(evidence: MutationDispatchEvidence { [weak self] evidence in
                self?.released(requestID, evidence: evidence)
            })
            let reference = UUID()
            entry.references.insert(reference)
            entry.evidence.reserve(by: owner)
            entries[requestID] = entry
            return MutationEvidenceLease(requestID: requestID, evidence: entry.evidence, ownerID: owner, referenceID: reference)
        }
    }

    func release(_ lease: MutationEvidenceLease) {
        entries.withLock { entries in
            guard var entry = entries[lease.requestID], entry.evidence === lease.evidence,
                  entry.references.remove(lease.referenceID) != nil else { return }
            if entry.references.isEmpty, !entry.retained, !entry.evidence.hasOwners { entries.removeValue(forKey: lease.requestID) }
            else { entries[lease.requestID] = entry }
        }
    }

    func retain(_ requestID: String) { entries.withLock { $0[requestID]?.retained = true } }
    func forget(_ requestID: String) {
        entries.withLock { entries in
            guard var entry = entries[requestID] else { return }
            entry.retained = false
            if entry.references.isEmpty, !entry.evidence.hasOwners { entries.removeValue(forKey: requestID) }
            else { entries[requestID] = entry }
        }
    }

    func retireActions(_ acknowledgment: ActionJournalAcknowledgment) {
        let retired = entries.withLock { $0.keys.filter { acknowledgment.retires($0) } }
        for requestID in retired { forget(requestID) }
    }

    private func released(_ requestID: String, evidence: MutationDispatchEvidence) {
        entries.withLock { entries in
            guard let entry = entries[requestID], entry.evidence === evidence,
                  entry.references.isEmpty, !entry.retained, !evidence.hasOwners else { return }
            entries.removeValue(forKey: requestID)
        }
    }
}
