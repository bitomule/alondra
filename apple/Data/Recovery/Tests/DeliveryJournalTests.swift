import AlondraCommands
import AlondraContracts
import AlondraRecovery
import Foundation
import Testing

@Suite struct DeliveryJournalTests {
    private let scope = CommandScope(server: URL(string: "https://relay.test")!, deviceID: "phone", credentialRevision: "mac-a", generation: 1)
    private let original = DeliveryAttempt(id: 7, sessionID: "session-a", channel: "menu_decision", state: "uncertain", version: 2, questionID: 42)

    @Test(arguments: ["missing-terminal", "missing-risk", "changed-version", "changed-session", "different-mac"])
    func unsafeDecisionNeverReachesMutation(mode: String) async throws {
        let fresh = DeliveryAttempt(id: 7, sessionID: mode == "changed-session" ? "session-b" : "session-a", channel: "menu_decision", state: "uncertain", version: mode == "changed-version" ? 3 : 2, questionID: 42)
        let journal = DeliveryJournal(scope: scope, macID: "mac-a", read: { [fresh] }, decide: { _, _ in
            Issue.record("Unsafe decision reached mutation"); return .confirmed
        })
        let observedScope = mode == "different-mac" ? CommandScope(server: scope.server, deviceID: scope.deviceID, credentialRevision: "mac-b", generation: 1) : scope
        do {
            _ = try await journal.resolve(original, in: observedScope, retry: true, terminalChecked: mode != "missing-terminal", duplicateRisk: mode != "missing-risk")
            Issue.record("Unsafe decision accepted")
        } catch { #expect(error is DeliveryJournalError) }
    }

    @Test func stoppedMacCannotDispatchAfterSuspendedReadReturns() async throws {
        let suspended = SuspendedDeliveryRead()
        let journal = DeliveryJournal(scope: scope, macID: "mac-a", read: { await suspended.read() }, decide: { _, _ in
            Issue.record("Deactivated Mac dispatched decision"); return .confirmed
        })
        let task = Task { try await journal.resolve(original, in: scope, retry: false, terminalChecked: true, duplicateRisk: false) }
        await suspended.started()
        await journal.stop()
        await suspended.release([original])
        do { _ = try await task.value; Issue.record("Deactivated result escaped") }
        catch { #expect(error is DeliveryJournalError) }
    }

    @Test func confirmedResolutionKeepsDestinationAndRefreshesAuthoritativeState() async throws {
        let resolved = DeliveryAttempt(id: 7, sessionID: "session-a", channel: "menu_decision", state: "retryable", version: 3, questionID: 42)
        let fixture = DeliveryReads(original, resolved)
        let journal = DeliveryJournal(scope: scope, macID: "mac-a", read: { await fixture.read() }, decide: { attempt, retry in
            #expect(attempt.id == 7 && attempt.version == 2 && attempt.sessionID == "session-a")
            #expect(retry)
            return .confirmed
        })
        let result = try await journal.resolve(original, in: scope, retry: true, terminalChecked: true, duplicateRisk: true)
        #expect(result.macID == "mac-a" && result.scope == scope)
        #expect(result.attempts == [resolved])
    }
}

private actor DeliveryReads {
    var values: [DeliveryAttempt]
    init(_ first: DeliveryAttempt, _ second: DeliveryAttempt) { values = [first, second] }
    func read() -> [DeliveryAttempt] { [values.removeFirst()] }
}
private actor SuspendedDeliveryRead {
    var waiter: CheckedContinuation<[DeliveryAttempt], Never>?
    var startedWaiter: CheckedContinuation<Void, Never>?
    func read() async -> [DeliveryAttempt] {
        await withCheckedContinuation { waiter = $0; startedWaiter?.resume(); startedWaiter = nil }
    }
    func started() async {
        if waiter != nil { return }
        await withCheckedContinuation { startedWaiter = $0 }
    }
    func release(_ attempts: [DeliveryAttempt]) { waiter?.resume(returning: attempts); waiter = nil }
}
