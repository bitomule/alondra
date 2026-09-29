import AlondraContracts
import AlondraRecovery
import Foundation
import Testing

private func attempt(version: UInt64 = 2, state: String = "uncertain") throws -> DomainV2.DeliveryAttempt {
    try DomainV2.decode(DomainV2.DeliveryAttempt.self, from: Data("""
    {"attempt_id":7,"message_id":3,"state":"\(state)","current_state":"\(state)","version":\(version),
     "reason":null,"recipient_kind":"agent","task_id":"t","interaction_id":"q"}
    """.utf8))
}

private actor Decisions {
    var count = 0
    func record() { count += 1 }
}

@Suite struct DeliveryJournalTests {
    private let scope = CommandScope(server: URL(string: "https://relay.test")!, deviceID: "phone", credentialRevision: "mac-a", generation: 1)

    @Test(arguments: ["missing-terminal", "missing-risk", "changed-version", "not-uncertain", "different-mac"])
    func unsafeDecisionNeverReachesTheServer(mode: String) async throws {
        let original = try attempt()
        let fresh = try attempt(version: mode == "changed-version" ? 3 : 2, state: mode == "not-uncertain" ? "retryable" : "uncertain")
        let decisions = Decisions()
        let journal = DeliveryJournal(scope: scope, macID: "mac-a", read: { [fresh] }, decide: { _, _ in
            await decisions.record()
            return .confirmed
        })
        let observed = mode == "different-mac"
            ? CommandScope(server: scope.server, deviceID: scope.deviceID, credentialRevision: "mac-b", generation: 1) : scope
        await #expect(throws: DeliveryJournalError.self) {
            _ = try await journal.resolve(original, in: observed, retry: true,
                                          terminalChecked: mode != "missing-terminal", duplicateRisk: mode != "missing-risk")
        }
        #expect(await decisions.count == 0)
    }

    @Test func aConfirmedDecisionReturnsTheRefreshedEvidence() async throws {
        let original = try attempt()
        let decisions = Decisions()
        let journal = DeliveryJournal(scope: scope, macID: "mac-a", read: { [original] }, decide: { _, retry in
            #expect(!retry)
            await decisions.record()
            return .confirmed
        })
        let snapshot = try await journal.resolve(original, in: scope, retry: false, terminalChecked: true, duplicateRisk: false)
        #expect(snapshot.attempts.map(\.attemptId) == [7])
        #expect(await decisions.count == 1)
    }

    @Test func anUnknownDecisionIsReportedAndNotRetried() async throws {
        let original = try attempt()
        let decisions = Decisions()
        let journal = DeliveryJournal(scope: scope, macID: "mac-a", read: { [original] }, decide: { _, _ in
            await decisions.record()
            return .unknown
        })
        await #expect(throws: DeliveryJournalError.outcomeUnknown) {
            _ = try await journal.resolve(original, in: scope, retry: true, terminalChecked: true, duplicateRisk: true)
        }
        #expect(await decisions.count == 1)
    }
}
