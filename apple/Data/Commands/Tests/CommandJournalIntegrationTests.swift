import Foundation
import Testing
import AlondraCommands

@Suite struct CommandJournalIntegrationTests {
    private let scope = CommandScope(
        server: URL(string: "https://example.test")!,
        deviceID: "phone-1",
        credentialRevision: "key-1",
        generation: 4
    )

    private func temporaryJournal() throws -> (URL, FileCommandJournal) {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("command-journal-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        return (directory, FileCommandJournal(directory: directory))
    }

    @Test func identifiedOperationUsesTheDurableReservationBeforeSending() async throws {
        let (directory, journal) = try temporaryJournal()
        defer { try? FileManager.default.removeItem(at: directory) }
        let gate = CommandGate(scope: scope, journal: journal)
        let result = await gate.submitIdentified(.markRead(42), itemID: "r42", in: scope,
                                                isUnknown: { _ in true }) { id in
            let records = try await journal.records()
            let pending = try #require(records.first { $0.command.id == id })
            #expect(pending.state == .pending)
            #expect(pending.command.kind == .markRead(42))
            throw URLError(.timedOut)
        }
        guard case .unknown(let original) = result else { Issue.record("Expected uncertain delivery"); return }
        let recreated = CommandGate(scope: scope, journal: FileCommandJournal(directory: directory))
        let repeated = await recreated.submitIdentified(.markRead(42), itemID: "r42", in: scope,
                                                       isUnknown: { _ in true }) { _ in
            Issue.record("A recreated service sent a second operation")
        }
        guard case .unknown(let restored) = repeated else { Issue.record("Lost durable identity"); return }
        #expect(restored.id == original.id)
    }

    @Test func uncertainPRQuestionBlocksOtherActionsAcrossGateRecreation() async throws {
        let (directory, journal) = try temporaryJournal()
        defer { try? FileManager.default.removeItem(at: directory) }
        let first = CommandGate(scope: scope, journal: journal)
        let result = await first.submitIdentified(.askOnPullRequest(42), itemID: "p42", in: scope,
                                                  isUnknown: { _ in true }) { _ in
            throw URLError(.timedOut)
        }
        guard case .unknown(let original) = result else { Issue.record("Lost uncertain PR question"); return }
        let secondWindow = CommandGate(scope: scope, journal: FileCommandJournal(directory: directory))
        for kind: CommandKind in [.requestChanges(42), .mergePullRequest(42), .resolveConflicts(42), .dismissPullRequest(42)] {
            let blocked = await secondWindow.submitIdentified(kind, itemID: "p42", in: scope,
                                                               isUnknown: { _ in true }) { _ in
                Issue.record("Another action bypassed the unresolved PR question")
            }
            guard case .unknown(let command) = blocked else { Issue.record("Lost cross-action PR exclusion"); return }
            #expect(command.id == original.id)
        }
        let independent = await secondWindow.submitIdentified(.mergePullRequest(43), itemID: "p43", in: scope,
                                                               isUnknown: { _ in true }) { _ in }
        guard case .confirmed = independent else { Issue.record("Unrelated PR was blocked"); return }
    }

    @Test func delayedDismissalCannotEraseWarningAfterScopeSwitch() async throws {
        let (directory, journal) = try temporaryJournal()
        defer { try? FileManager.default.removeItem(at: directory) }
        let command = UncertainCommand(id: UUID(), scope: scope, kind: .newRequest, itemID: nil)
        try await journal.save(.init(command: command, state: .unknown))
        let gate = CommandGate(scope: scope, journal: journal)
        #expect(try await gate.restore() == [command])
        let other = CommandScope(server: URL(string: "https://other.example")!, deviceID: "phone-1",
            credentialRevision: "key-2", generation: 5)
        await gate.use(other)
        await gate.dismiss(command.id, in: scope)
        #expect(try await journal.records().contains { $0.command.id == command.id })
    }


    @Test func pendingAtCrashBecomesUnknownAndCannotSendAgainAfterRelaunch() async throws {
        let (directory, journal) = try temporaryJournal()
        defer { try? FileManager.default.removeItem(at: directory) }
        let command = UncertainCommand(id: UUID(), scope: scope, kind: .send("node-1"), itemID: "card-1")
        try await journal.save(.init(command: command, state: .pending))

        // A new actor models a process restart with a generation counter reset to zero.
        let relaunchedScope = CommandScope(server: scope.server, deviceID: scope.deviceID,
            credentialRevision: scope.credentialRevision, generation: 0)
        let gate = CommandGate(scope: relaunchedScope, journal: journal)
        #expect(try await gate.restore() == [command])
        let result = await gate.submit(.send("node-1"), itemID: "card-1", in: relaunchedScope,
            isUnknown: { _ in true }) {
            Issue.record("A pending command was replayed after restart")
        }
        guard case .unknown(let existing) = result else {
            Issue.record("The persisted pending send was forgotten")
            return
        }
        #expect(existing.id == command.id)
    }

    @Test func lostResponsePersistsOnlyMetadataAndSecondAttemptDoesNotSend() async throws {
        let (directory, journal) = try temporaryJournal()
        defer { try? FileManager.default.removeItem(at: directory) }
        let gate = CommandGate(scope: scope, journal: journal)
        let first = await gate.submit(.newRequest, itemID: "request-1", in: scope,
            isUnknown: { _ in true }) {
            let secretPayload = "private request body"
            _ = secretPayload
            throw URLError(.timedOut)
        }
        guard case .unknown(let uncertain) = first else {
            Issue.record("Lost response must be uncertain")
            return
        }
        let second = await gate.submit(.newRequest, itemID: "request-1", in: scope,
            isUnknown: { _ in true }) {
            Issue.record("A second attempt sent the same unresolved command")
        }
        guard case .unknown(let duplicate) = second else {
            Issue.record("Second attempt was not blocked")
            return
        }
        #expect(duplicate.id == uncertain.id)

        let file = directory.appendingPathComponent(uncertain.id.uuidString).appendingPathExtension("json")
        let bytes = try Data(contentsOf: file)
        let text = try #require(String(data: bytes, encoding: .utf8))
        #expect(!text.contains("private request body"))
        #expect(try await journal.records().count == 1)
        let relaunched = CommandGate(scope: scope, journal: journal)
        #expect(try await relaunched.restore() == [uncertain])
    }

    @Test func unresolvedCardBlocksDifferentNotificationActionAfterRelaunch() async throws {
        let (directory, journal) = try temporaryJournal()
        defer { try? FileManager.default.removeItem(at: directory) }
        let gate = CommandGate(scope: scope, journal: journal)
        let first = await gate.submit(.moreContext(7), itemID: "q7", in: scope,
            isUnknown: { _ in true }) { throw URLError(.timedOut) }
        guard case .unknown(let original) = first else {
            Issue.record("Lost response was not retained")
            return
        }
        let relaunched = CommandGate(scope: scope, journal: FileCommandJournal(directory: directory))
        let conflicting = await relaunched.submit(.answer(7), itemID: "q7", in: scope,
            isUnknown: { _ in true }) {
            Issue.record("A different button sent a second command for an unresolved card")
        }
        guard case .unknown(let blocked) = conflicting else {
            Issue.record("Conflicting card action was not blocked")
            return
        }
        #expect(blocked.id == original.id)
        let otherCard = await relaunched.submit(.answer(8), itemID: "q8", in: scope,
            isUnknown: { _ in true }) {}
        guard case .confirmed = otherCard else {
            Issue.record("An unrelated card was blocked")
            return
        }
    }

    @Test func confirmedAttemptDoesNotBecomeUnknownAfterRelaunch() async throws {
        let (directory, journal) = try temporaryJournal()
        defer { try? FileManager.default.removeItem(at: directory) }
        let gate = CommandGate(scope: scope, journal: journal)
        let result = await gate.submit(.answer(7), itemID: "card-7", in: scope,
            isUnknown: { _ in true }) {}
        guard case .confirmed = result else {
            Issue.record("Successful command was not confirmed")
            return
        }
        let relaunched = CommandGate(scope: scope, journal: journal)
        #expect(try await relaunched.restore().isEmpty)
        #expect(try await journal.records().isEmpty)
    }

    @Test func concurrentSecondAttemptCannotPassWhileFirstSendIsInFlight() async throws {
        let (directory, journal) = try temporaryJournal()
        defer { try? FileManager.default.removeItem(at: directory) }
        let gate = CommandGate(scope: scope, journal: journal)
        let started = AsyncStream.makeStream(of: Void.self)
        let release = AsyncStream.makeStream(of: Void.self)
        let first = Task {
            await gate.submit(.markRead(42), itemID: "card-42", in: scope,
                isUnknown: { _ in true }) {
                started.continuation.yield(())
                for await _ in release.stream { break }
                throw URLError(.timedOut)
            }
        }
        for await _ in started.stream { break }
        let secondGate = CommandGate(scope: scope, journal: FileCommandJournal(directory: directory))
        let second = await secondGate.submit(.markRead(42), itemID: "card-42", in: scope,
            isUnknown: { _ in true }) {
            Issue.record("Concurrent duplicate reached the transport")
        }
        guard case .unknown(let blocked) = second else {
            Issue.record("Concurrent duplicate was not blocked")
            release.continuation.yield(())
            _ = await first.value
            return
        }
        release.continuation.yield(())
        guard case .unknown(let final) = await first.value else {
            Issue.record("First lost response was not uncertain")
            return
        }
        #expect(blocked.id == final.id)
    }

    @Test func unreadableJournalFailsClosedBeforeTransport() async throws {
        let (directory, journal) = try temporaryJournal()
        defer { try? FileManager.default.removeItem(at: directory) }
        try Data("not JSON".utf8).write(to: directory.appendingPathComponent("broken.json"))
        let gate = CommandGate(scope: scope, journal: journal)
        let result = await gate.submit(.newRequest, in: scope, isUnknown: { _ in true }) {
            Issue.record("Transport ran despite unreadable journal")
        }
        guard case .rejected = result else {
            Issue.record("Unreadable journal did not fail closed")
            return
        }
    }
}
