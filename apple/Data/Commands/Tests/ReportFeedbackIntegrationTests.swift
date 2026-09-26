import AlondraCommands
import Foundation
import Testing

@Suite struct ReportFeedbackIntegrationTests {
    private let scope = CommandScope(server: URL(string: "https://example.test")!,
        deviceID: "client-1", credentialRevision: "revision", generation: 0)

    @Test func recreationCompletesOnlyReadWithOriginalIdentityAndNeverRepeatsMessage() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let journalURL = directory.appendingPathComponent("commands")
        let feedbackURL = directory.appendingPathComponent("feedback")
        let gate = CommandGate(scope: scope, journal: FileCommandJournal(directory: journalURL))
        let first = ReportFeedbackTransaction(directory: feedbackURL, gate: gate, scope: scope)
        let transport = FeedbackTransport()
        let result = await first.submit(reportID: 42,
            sendMessage: { await transport.message($0) },
            markRead: { try await transport.read($0) }, reconcile: { _ in false })
        guard case .unknown(let reserved) = result else { Issue.record("Lost uncertain read"); return }
        let restoredGate = CommandGate(scope: scope, journal: FileCommandJournal(directory: journalURL))
        let relaunched = ReportFeedbackTransaction(directory: feedbackURL, gate: restoredGate, scope: scope)
        let recovered = await relaunched.submit(reportID: 42,
            sendMessage: { _ in Issue.record("Replayed report feedback after restart") },
            markRead: { try await transport.read($0) }, reconcile: { _ in
                Issue.record("Message was already confirmed before restart"); return false
            })
        guard case .confirmed = recovered else { Issue.record("Read never recovered"); return }
        #expect(await transport.messages == [reserved.id.uuidString + "-message"])
        #expect(await transport.reads == [reserved.id.uuidString + "-read", reserved.id.uuidString + "-read"])
        #expect(try await restoredGate.restore().isEmpty)
        let completedAgain = await relaunched.submit(reportID: 42,
            sendMessage: { _ in Issue.record("Completed transaction replayed message") },
            markRead: { _ in Issue.record("Completed transaction replayed read") }, reconcile: { _ in false })
        guard case .confirmed = completedAgain else { Issue.record("Lost completed checkpoint"); return }
    }

    @Test func uncertainMessageMustBeConfirmedByStatusBeforeReadCanStart() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let journalURL = directory.appendingPathComponent("commands")
        let feedbackURL = directory.appendingPathComponent("feedback")
        let first = ReportFeedbackTransaction(directory: feedbackURL,
            gate: CommandGate(scope: scope, journal: FileCommandJournal(directory: journalURL)), scope: scope)
        let result = await first.submit(reportID: 7,
            sendMessage: { _ in throw URLError(.timedOut) },
            markRead: { _ in Issue.record("Read started with unconfirmed message") }, reconcile: { _ in false })
        guard case .unknown(let original) = result else { Issue.record("Expected unknown send"); return }
        let restored = ReportFeedbackTransaction(directory: feedbackURL,
            gate: CommandGate(scope: scope, journal: FileCommandJournal(directory: journalURL)), scope: scope)
        let unresolved = await restored.submit(reportID: 7,
            sendMessage: { _ in Issue.record("Uncertain message replayed") },
            markRead: { _ in Issue.record("Uncertain message marked read") }, reconcile: { id in
                #expect(id == original.id.uuidString + "-message"); return false
            })
        guard case .unknown = unresolved else { Issue.record("Forgot uncertainty"); return }
        let resolved = await restored.submit(reportID: 7,
            sendMessage: { _ in Issue.record("Message replayed during reconciliation") },
            markRead: { id in #expect(id == original.id.uuidString + "-read") }, reconcile: { id in
                #expect(id == original.id.uuidString + "-message"); return true
            })
        guard case .confirmed = resolved else { Issue.record("Status-confirmed feedback failed to finish"); return }
    }
}

private actor FeedbackTransport {
    var messages: [String] = []
    var reads: [String] = []
    func message(_ id: String) { messages.append(id) }
    func read(_ id: String) throws {
        reads.append(id)
        if reads.count == 1 { throw URLError(.timedOut) }
    }
}
