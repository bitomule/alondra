import AlondraRecovery
import AlondraCommands
import Foundation
import Testing
import Synchronization

@Suite struct DeliveryStatusIntegrationTests {
    private var scope: CommandScope {
        CommandScope(server: URL(string: "https://example.test")!, deviceID: "phone", credentialRevision: "key", generation: 1)
    }
    @Test(.timeLimit(.minutes(1)))
    func journalReadFailureRecoversWithoutReopeningTheSceneOrResending() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let journal = FileCommandJournal(directory: directory.appendingPathComponent("commands"))
        let original = CommandGate(scope: scope, journal: journal)
        let result = await original.submitIdentified(.newRequest, in: scope, isUnknown: { _ in true }) { _ in
            throw URLError(.timedOut)
        }
        guard case .unknown(let command) = result else { Issue.record("Expected uncertainty"); return }
        let interrupted = InterruptedJournal(base: journal, readFailures: 2)
        let relaunched = CommandGate(scope: scope, journal: interrupted)
        let service = DeliveryStatusService(gate: relaunched, scope: scope,
            storage: .file(directory.appendingPathComponent("warnings.json")), retryDelay: { await Task.yield() })
        var updates = await service.observe().makeAsyncIterator()
        var recovered = false
        while let update = await updates.next() {
            if update.operations.contains(where: { $0.id == command.id }) {
                #expect(update.persistenceError == nil)
                recovered = true
                break
            }
        }
        #expect(recovered)
        #expect(await interrupted.failedReads == 2)
        let duplicate = await relaunched.submitIdentified(.newRequest, in: scope, isUnknown: { _ in true }) { _ in
            Issue.record("Journal read recovery must not resend the request")
        }
        guard case .unknown(let retained) = duplicate else { Issue.record("Expected retained reservation"); await service.stop(); return }
        #expect(retained.id == command.id)
        await service.stop()
    }

    @Test(.timeLimit(.minutes(1)))
    func feedbackSnapshotCannotClearUnreadableCommandJournal() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let journal = FileCommandJournal(directory: directory.appendingPathComponent("commands"))
        let original = CommandGate(scope: scope, journal: journal)
        let result = await original.submitIdentified(.newRequest, in: scope, isUnknown: { _ in true }) { _ in
            throw URLError(.timedOut)
        }
        guard case .unknown(let command) = result else { Issue.record("Expected uncertainty"); return }
        let interrupted = InterruptedJournal(base: journal, readFailures: 2)
        let retry = DeliveryRetryGate()
        let feedback = AsyncStream<[ReportFeedbackRecoveryEvent]>.makeStream()
        let service = DeliveryStatusService(gate: CommandGate(scope: scope, journal: interrupted), scope: scope,
            storage: .file(directory.appendingPathComponent("warnings.json")),
            retryDelay: { await retry.wait() }, feedback: { feedback.stream })
        var updates = await service.observe().makeAsyncIterator()
        // Both journal failures have been published; no further reads happen before release.
        await retry.started()
        let failed = try #require(await updates.next())
        #expect(failed.persistenceError != nil)
        feedback.continuation.yield([])
        let feedbackUpdate = try #require(await updates.next())
        #expect(feedbackUpdate.persistenceError == failed.persistenceError,
                "Successful feedback/metadata work cannot imply that command-journal recovery succeeded")
        await retry.release()
        let recovered = try #require(await updates.next())
        #expect(recovered.persistenceError == nil)
        #expect(recovered.operations.map(\.id) == [command.id])
        await service.stop()
        feedback.continuation.finish()
    }

    @Test func warningAcknowledgementSurvivesRelaunchWithoutUnlockingOrResending() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let journal = FileCommandJournal(directory: directory.appendingPathComponent("commands"))
        let first = CommandGate(scope: scope, journal: journal)
        let result = await first.submitIdentified(.newRequest, in: scope, isUnknown: { _ in true }) { _ in throw URLError(.timedOut) }
        guard case .unknown(let command) = result else { Issue.record("Expected uncertainty"); return }
        let file = directory.appendingPathComponent("warnings.json")
        let service = DeliveryStatusService(gate: first, scope: scope, storage: .file(file), now: { Date(timeIntervalSince1970: 123) })
        let initial = await service.refresh()
        #expect(initial.operations.map(\.id) == [command.id])
        try await service.acknowledge([command.id: .unconfirmed], in: scope)
        await service.stop()
        let relaunchedGate = CommandGate(scope: scope, journal: journal)
        let relaunched = DeliveryStatusService(gate: relaunchedGate, scope: scope, storage: .file(file), now: { Date(timeIntervalSince1970: 999) })
        let restored = await relaunched.refresh()
        #expect(restored.operations.first?.acknowledged == true)
        #expect(restored.operations.first?.firstObservedUncertainAt == Date(timeIntervalSince1970: 123))
        let retry = await relaunchedGate.submitIdentified(.newRequest, in: scope, isUnknown: { _ in true }) { _ in
            Issue.record("Acknowledging a warning must never authorize another POST")
        }
        guard case .unknown(let same) = retry else { Issue.record("Expected original block"); return }
        #expect(same.id == command.id)
        let recovery = CommandRecovery(gate: relaunchedGate, scope: scope,
            file: directory.appendingPathComponent("resolutions.json"), status: { id in
                #expect(id == command.id)
                return 201
            })
        await recovery.recoverOnce()
        #expect(await relaunched.refresh().operations.isEmpty)
        await relaunched.stop()
        await recovery.stop()
    }

    @Test(.timeLimit(.minutes(1))) func feedbackReadRejectionIsOneLogicalOperationAndNeverClaimsRetry() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let gate = CommandGate(scope: scope, journal: FileCommandJournal(directory: directory.appendingPathComponent("commands")))
        let feedback = ReportFeedbackTransaction(directory: directory.appendingPathComponent("feedback"), gate: gate, scope: scope)
        _ = await feedback.submit(reportID: 4, binding: Data([1]), sendMessage: { _ in
            Issue.record("Already confirmed message must not be resent")
        }, markRead: { _ in
            Issue.record("Rejected read requires explicit retry")
        }, reconcile: { id in id.hasSuffix("-message") ? .confirmed : .rejected(403) })
        let service = DeliveryStatusService(gate: gate, scope: scope,
            storage: .file(directory.appendingPathComponent("warnings.json")),
            feedback: { await feedback.recoveryEvents() })
        var updates = await service.observe().makeAsyncIterator()
        let initial = try #require(await updates.next())
        let operation = try #require(initial.operations.first)
        #expect(operation.phase == .unconfirmed)
        let observed = [operation.id: operation.phase]
        _ = try await feedback.recover(markRead: { _, _ in
            Issue.record("Passive recovery must not retry a rejected read")
        }, reconcile: { id in id.hasSuffix("-message") ? .confirmed : .rejected(403) })
        while let update = await updates.next() {
            if update.operations.contains(where: { $0.phase == .feedbackReadRejected(403) }) {
                #expect(update.operations.count == 1)
                #expect(update.operations.first?.acknowledged == false)
                // A delayed tap from the old warning must not hide the newly received failure.
                try await service.acknowledge(observed, in: scope)
                #expect(await service.refresh().operations.first?.acknowledged == false)
                try await service.acknowledge([operation.id: .feedbackReadRejected(403)], in: scope)
                #expect(await service.refresh().operations.first?.acknowledged == true)
                break
            }
        }
        await service.stop()
    }

    @Test func failedAcknowledgementAndOldScopeKeepUncertainty() async throws {
        let gate = CommandGate(scope: scope)
        let result = await gate.submitIdentified(.send("node"), in: scope, isUnknown: { _ in true }) { _ in throw URLError(.timedOut) }
        guard case .unknown(let command) = result else { Issue.record("Expected uncertainty"); return }
        let clock = Mutex(Date(timeIntervalSince1970: 123))
        let service = DeliveryStatusService(gate: gate, scope: scope,
            storage: .init(load: { [] }, save: { _ in throw CocoaError(.fileWriteNoPermission) }),
            now: { clock.withLock { $0 } })
        #expect(await service.refresh().persistenceError != nil)
        clock.withLock { $0 = Date(timeIntervalSince1970: 999) }
        #expect(await service.refresh().operations.first?.firstObservedUncertainAt == Date(timeIntervalSince1970: 123),
                "A metadata write failure must not keep resetting the warning's age")
        do { try await service.acknowledge([command.id: .unconfirmed], in: scope); Issue.record("Expected write failure") } catch {}
        #expect(await service.refresh().operations.first?.acknowledged == false)
        let next = CommandScope(server: scope.server, deviceID: scope.deviceID, credentialRevision: "new-key", generation: 2)
        await gate.use(next)
        do { try await service.acknowledge([command.id: .unconfirmed], in: scope); Issue.record("Expected stale-scope rejection") } catch {}
        await service.stop()
    }
}

private actor InterruptedJournal: CommandJournaling {
    let base: FileCommandJournal
    private var readFailures: Int
    private(set) var failedReads = 0
    init(base: FileCommandJournal, readFailures: Int) { self.base = base; self.readFailures = readFailures }
    func records() async throws -> [CommandJournalRecord] {
        if readFailures > 0 {
            readFailures -= 1
            failedReads += 1
            throw CocoaError(.fileReadNoPermission)
        }
        return try await base.records()
    }
    func reserve(_ command: UncertainCommand) async throws -> CommandJournalReservation { try await base.reserve(command) }
    func save(_ record: CommandJournalRecord) async throws { try await base.save(record) }
    func remove(_ id: UUID) async throws { try await base.remove(id) }
}

private actor DeliveryRetryGate {
    private var continuation: CheckedContinuation<Void, Never>?
    private var startWaiters: [CheckedContinuation<Void, Never>] = []
    func wait() async {
        await withCheckedContinuation { continuation in
            self.continuation = continuation
            for waiter in startWaiters { waiter.resume() }
            startWaiters = []
        }
    }
    func started() async {
        if continuation != nil { return }
        await withCheckedContinuation { startWaiters.append($0) }
    }
    func release() { continuation?.resume(); continuation = nil }
}
