import AlondraCommands
import Foundation
import Testing

@Suite struct ReportFeedbackIntegrationTests {
    private let scope = CommandScope(server: URL(string: "https://example.test")!,
        deviceID: "client-1", credentialRevision: "revision", generation: 0)
    private let binding = Data("keyed-draft-binding".utf8)

    private func transaction(_ directory: URL) -> ReportFeedbackTransaction {
        ReportFeedbackTransaction(directory: directory.appendingPathComponent("feedback"),
            gate: CommandGate(scope: scope, journal: FileCommandJournal(directory: directory.appendingPathComponent("commands"))),
            scope: scope)
    }

    @Test func recreationFinishesReadByStatusWithoutRepeatingEitherDispatch() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let remote = FeedbackTransport()
        let result = await transaction(directory).submit(reportID: 42, binding: binding,
            sendMessage: { await remote.message($0) }, markRead: { try await remote.uncertainRead($0) },
            reconcile: { await remote.status($0) })
        guard case .unknown(let original) = result else { Issue.record("Expected unknown read"); return }
        await remote.resolve(original.id.uuidString + "-read", to: .confirmed)
        let recovered = await transaction(directory).submit(reportID: 42, binding: binding,
            sendMessage: { _ in Issue.record("Replayed confirmed message") },
            markRead: { _ in Issue.record("Replayed previously dispatched read") },
            reconcile: { await remote.status($0) })
        guard case .confirmed = recovered else { Issue.record("Read did not recover"); return }
        #expect(await remote.messages == [original.id.uuidString + "-message"])
        #expect(await remote.reads == [original.id.uuidString + "-read"])
        #expect(try await FileCommandJournal(directory: directory.appendingPathComponent("commands")).records().isEmpty)
        let differentDraft = await transaction(directory).submit(reportID: 42, binding: Data("another-draft".utf8),
            sendMessage: { _ in Issue.record("New draft bypassed retained transaction") },
            markRead: { _ in Issue.record("New draft changed read state") }, reconcile: { await remote.status($0) })
        guard case .rejected = differentDraft else { Issue.record("Unsent draft was reported sent"); return }
    }

    @Test func unknownMessageBlocksReadUntilAuthoritativeConfirmation() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let remote = FeedbackTransport()
        let result = await transaction(directory).submit(reportID: 7, binding: binding,
            sendMessage: { try await remote.uncertainMessage($0) },
            markRead: { _ in Issue.record("Read before message confirmation") }, reconcile: { await remote.status($0) })
        guard case .unknown(let original) = result else { Issue.record("Expected unknown send"); return }
        let unresolved = await transaction(directory).submit(reportID: 7, binding: binding,
            sendMessage: { _ in Issue.record("Replayed unknown message") },
            markRead: { _ in Issue.record("Read unconfirmed feedback") }, reconcile: { await remote.status($0) })
        guard case .unknown = unresolved else { Issue.record("Forgot unknown send"); return }
        await remote.resolve(original.id.uuidString + "-message", to: .confirmed)
        let recovered = await transaction(directory).submit(reportID: 7, binding: binding,
            sendMessage: { _ in Issue.record("Replayed status-confirmed message") },
            markRead: { await remote.read($0) }, reconcile: { await remote.status($0) })
        guard case .confirmed = recovered else { Issue.record("Could not finish confirmed feedback"); return }
        #expect(await remote.messages == [original.id.uuidString + "-message"])
        #expect(await remote.reads == [original.id.uuidString + "-read"])
    }

    @Test(arguments: [false, true])
    func crashBeforeDispatchCanResumeAfterProvenOutboxAbsence(checkpointWritten: Bool) async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let journal = FileCommandJournal(directory: directory.appendingPathComponent("commands"))
        let id: UUID
        if checkpointWritten {
            let result = await transaction(directory).submit(reportID: 9, binding: binding,
                sendMessage: { _ in throw URLError(.userAuthenticationRequired) },
                markRead: { _ in Issue.record("Read despite no message dispatch") }, reconcile: { _ in .notDispatched })
            guard case .unknown(let command) = result else { Issue.record("Expected reserved operation"); return }
            id = command.id
        } else {
            id = UUID()
            _ = try await journal.reserve(UncertainCommand(id: id, scope: scope,
                kind: .replyToReport(9), itemID: "r9"))
        }
        let remote = FeedbackTransport()
        let recovered = await transaction(directory).submit(reportID: 9, binding: binding,
            sendMessage: { await remote.message($0) }, markRead: { await remote.read($0) },
            reconcile: { await remote.status($0) })
        guard case .confirmed = recovered else { Issue.record("Pre-dispatch crash blocked forever"); return }
        #expect(await remote.messages == [id.uuidString + "-message"])
        #expect(await remote.reads == [id.uuidString + "-read"])
    }

    @Test func rejectedReadRetriesOnlyReadWithFreshIdentity() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let remote = FeedbackTransport()
        let result = await transaction(directory).submit(reportID: 12, binding: binding,
            sendMessage: { await remote.message($0) },
            markRead: { id in await remote.resolve(id, to: .rejected(422)); throw URLError(.badServerResponse) },
            reconcile: { await remote.status($0) })
        guard case .rejected = result else { Issue.record("Read rejection was not surfaced"); return }
        let original = try #require(try await FileCommandJournal(directory: directory.appendingPathComponent("commands")).records().first)
        let resultAfterRetry = await transaction(directory).submit(reportID: 12, binding: binding,
            sendMessage: { _ in Issue.record("Repeated message after read rejection") },
            markRead: { await remote.read($0) }, reconcile: { await remote.status($0) })
        guard case .confirmed = resultAfterRetry else { Issue.record("Explicit read retry did not finish"); return }
        #expect(await remote.messages == [original.command.id.uuidString + "-message"])
        #expect(await remote.reads == [original.command.id.uuidString + "-read-1"])
    }

    @Test(arguments: [404, 422])
    func definitiveMessageFailureReleasesGateWithoutMarkingRead(status: Int) async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let remote = FeedbackTransport()
        let result = await transaction(directory).submit(reportID: 8, binding: binding,
            sendMessage: { id in await remote.resolve(id, to: .rejected(status)); throw URLError(.badServerResponse) },
            markRead: { _ in Issue.record("Rejected feedback marked read") }, reconcile: { await remote.status($0) })
        guard case .rejected = result else { Issue.record("Definitive rejection became permanent uncertainty"); return }
        #expect(try await FileCommandJournal(directory: directory.appendingPathComponent("commands")).records().isEmpty)
        let corrected = await transaction(directory).submit(reportID: 8, binding: Data("corrected draft".utf8),
            sendMessage: { await remote.message($0) }, markRead: { await remote.read($0) }, reconcile: { await remote.status($0) })
        guard case .confirmed = corrected else { Issue.record("Explicit corrected submit remained blocked"); return }
    }
}

private actor FeedbackTransport {
    var messages: [String] = []
    var reads: [String] = []
    var statuses: [String: ReportFeedbackStatus] = [:]
    func status(_ id: String) -> ReportFeedbackStatus { statuses[id] ?? .notDispatched }
    func resolve(_ id: String, to status: ReportFeedbackStatus) { statuses[id] = status }
    func message(_ id: String) { messages.append(id); statuses[id] = .confirmed }
    func read(_ id: String) { reads.append(id); statuses[id] = .confirmed }
    func uncertainMessage(_ id: String) throws {
        messages.append(id); statuses[id] = .unknown; throw URLError(.timedOut)
    }
    func uncertainRead(_ id: String) throws {
        reads.append(id); statuses[id] = .unknown; throw URLError(.timedOut)
    }
}
