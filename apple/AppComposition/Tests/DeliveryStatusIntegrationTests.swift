import AlondraAppComposition
import AlondraCommands
import Foundation
import Testing

@Suite struct DeliveryStatusIntegrationTests {
    private var scope: CommandScope {
        CommandScope(server: URL(string: "https://example.test")!, deviceID: "phone", credentialRevision: "key", generation: 1)
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
        try await service.acknowledge([command.id], in: scope)
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
        _ = try await feedback.recover(markRead: { _, _ in
            Issue.record("Passive recovery must not retry a rejected read")
        }, reconcile: { id in id.hasSuffix("-message") ? .confirmed : .rejected(403) })
        let service = DeliveryStatusService(gate: gate, scope: scope,
            storage: .file(directory.appendingPathComponent("warnings.json")),
            feedback: { await feedback.recoveryEvents() })
        var updates = await service.observe().makeAsyncIterator()
        while let update = await updates.next() {
            if update.operations.contains(where: { $0.phase == .feedbackReadRejected(403) }) {
                #expect(update.operations.count == 1)
                #expect(update.operations.first?.acknowledged == false)
                break
            }
        }
        await service.stop()
    }

    @Test func failedAcknowledgementAndOldScopeKeepUncertainty() async throws {
        let gate = CommandGate(scope: scope)
        let result = await gate.submitIdentified(.send("node"), in: scope, isUnknown: { _ in true }) { _ in throw URLError(.timedOut) }
        guard case .unknown(let command) = result else { Issue.record("Expected uncertainty"); return }
        let service = DeliveryStatusService(gate: gate, scope: scope, storage: .init(load: { [] }, save: { _ in throw CocoaError(.fileWriteNoPermission) }))
        #expect(await service.refresh().persistenceError != nil)
        do { try await service.acknowledge([command.id], in: scope); Issue.record("Expected write failure") } catch {}
        #expect(await service.refresh().operations.first?.acknowledged == false)
        let next = CommandScope(server: scope.server, deviceID: scope.deviceID, credentialRevision: "new-key", generation: 2)
        await gate.use(next)
        do { try await service.acknowledge([command.id], in: scope); Issue.record("Expected stale-scope rejection") } catch {}
        await service.stop()
    }
}
