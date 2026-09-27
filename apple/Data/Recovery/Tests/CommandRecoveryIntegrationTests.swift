import AlondraRecovery
import AlondraCommands
import AlondraContracts
import Foundation
import Testing

@Suite struct CommandRecoveryIntegrationTests {
    @Test func validVersionedDecisionRecoversWithoutAnotherDispatch() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let scope = CommandScope(server: URL(string: "https://example.test")!, deviceID: "client", credentialRevision: "mac-a", generation: 0)
        let gate = CommandGate(scope: scope, journal: FileCommandJournal(directory: directory.appendingPathComponent("commands")))
        let decision = DeliveryDecision(observed: .init(id: 7, sessionID: "s", channel: "hook_decision", state: "uncertain", version: 2), retry: true)
        let body = Data(#"{"attempt":{"id":7,"session_id":"s","channel":"hook_decision","state":"retryable","version":3}}"#.utf8)
        try decision.validate(body)
        let first = await gate.submitIdentified(.resolveDelivery(decision), in: scope, isUnknown: { _ in true }) { _ in throw URLError(.timedOut) }
        guard case .unknown(let command) = first else { Issue.record("Expected lost reply"); return }
        let recovery = CommandRecovery(gate: gate, scope: scope, file: directory.appendingPathComponent("resolutions.json"), response: { id in
            #expect(id == command.id)
            return .init(status: 200, body: body)
        })
        await recovery.recoverOnce()
        #expect(try await gate.restore().isEmpty)
        await recovery.stop()
    }

    @Test(arguments: ["wrong-id", "wrong-session", "wrong-channel", "wrong-version", "wrong-state", "malformed"])
    func invalidDirectDeliveryDecisionCannotBecomeSuccessByRecoveringTheSameResponse(mode: String) async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let scope = CommandScope(server: URL(string: "https://example.test")!, deviceID: "client", credentialRevision: "mac-a", generation: 0)
        let gate = CommandGate(scope: scope, journal: FileCommandJournal(directory: directory.appendingPathComponent("commands")))
        let decision = DeliveryDecision(observed: .init(id: 7, sessionID: "s", channel: "hook_decision", state: "uncertain", version: 2), retry: true)
        let id = mode == "wrong-id" ? 8 : 7
        let session = mode == "wrong-session" ? "other" : "s"
        let channel = mode == "wrong-channel" ? "terminal" : "hook_decision"
        let version = mode == "wrong-version" ? 4 : 3
        let state = mode == "wrong-state" ? "handed" : "retryable"
        let body = Data((mode == "malformed" ? "not-json" : "{\"attempt\":{\"id\":\(id),\"session_id\":\"\(session)\",\"channel\":\"\(channel)\",\"state\":\"\(state)\",\"version\":\(version)}}").utf8)
        let first = await gate.submitIdentified(.resolveDelivery(decision), in: scope, isUnknown: { _ in true }) { _ in
            try decision.validate(body)
        }
        guard case .unknown(let command) = first else { Issue.record("Invalid direct reply accepted"); return }
        let file = directory.appendingPathComponent("resolutions.json")
        let recovery = CommandRecovery(gate: gate, scope: scope, file: file, response: { _ in .init(status: 200, body: body) })
        await recovery.recoverOnce()
        #expect(try await gate.restore().map(\.id) == [command.id], "Reconciliation cannot erase the direct validator's refusal")
        #expect(!FileManager.default.fileExists(atPath: file.path))
        await recovery.stop()
    }

    @Test(arguments: ["not-json", "[]", ""])
    func malformedRecoveredAnswerRemainsBlocked(body: String) async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let scope = CommandScope(server: URL(string: "https://example.test")!, deviceID: "client", credentialRevision: "mac-a", generation: 0)
        let gate = CommandGate(scope: scope, journal: FileCommandJournal(directory: directory.appendingPathComponent("commands")))
        let first = await gate.submitIdentified(.answer(42), in: scope, isUnknown: { _ in true }) { _ in throw URLError(.timedOut) }
        guard case .unknown(let command) = first else { Issue.record("Expected lost reply"); return }
        let recovery = CommandRecovery(gate: gate, scope: scope, file: directory.appendingPathComponent("resolutions.json"), response: { _ in .init(status: 200, body: Data(body.utf8)) })
        await recovery.recoverOnce()
        #expect(try await gate.restore().map(\.id) == [command.id])
        await recovery.stop()
    }

    @Test(.timeLimit(.minutes(1))) func recoveredHTTP200PersistsUncertainHandoffInsteadOfSuccess() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let scope = CommandScope(server: URL(string: "https://example.test")!, deviceID: "client", credentialRevision: "mac-a", generation: 0)
        let gate = CommandGate(scope: scope, journal: FileCommandJournal(directory: directory.appendingPathComponent("commands")))
        let initial = await gate.submitIdentified(.answer(42), in: scope, isUnknown: { _ in true }) { _ in throw URLError(.timedOut) }
        guard case .unknown(let command) = initial else { Issue.record("Expected lost response"); return }
        let file = directory.appendingPathComponent("resolutions.json")
        let recovery = CommandRecovery(gate: gate, scope: scope, file: file, response: { id in
            #expect(id == command.id)
            return CommandResponse(status: 200, body: Data(#"{"delivery_state":"uncertain","delivery_attempt":77}"#.utf8))
        })
        await recovery.recoverOnce()
        #expect(try await gate.restore().isEmpty, "Transport receipt is confirmed, not an invitation to replay the POST")
        await recovery.stop()
        let restarted = CommandRecovery(gate: gate, scope: scope, file: file, response: { _ in
            Issue.record("Persisted handoff classification was lost"); return nil
        })
        var stream = await restarted.subscribe().makeAsyncIterator()
        let resolution = try #require(await stream.next())
        #expect(resolution.status == 200)
        #expect(!resolution.succeeded)
        #expect(resolution.deliveryUnconfirmed == true)
        #expect(resolution.deliveryAttemptID == 77)
        await restarted.stop()
    }

    @Test(.timeLimit(.minutes(1))) func relaunchRecoversOnlyTerminalStatusAndReplaysWithoutDispatch() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let scope = CommandScope(server: URL(string: "https://example.test")!, deviceID: "client-1", credentialRevision: "revision", generation: 0)
        let journalDirectory = directory.appendingPathComponent("commands")
        let first = CommandGate(scope: scope, journal: FileCommandJournal(directory: journalDirectory))
        let result = await first.submitIdentified(.newRequest, in: scope, isUnknown: { _ in true }) { _ in
            throw URLError(.timedOut)
        }
        guard case .unknown(let original) = result else { Issue.record("Expected uncertain command"); return }
        let gate = CommandGate(scope: scope, journal: FileCommandJournal(directory: journalDirectory))
        let status = StatusProbe()
        let recoveryFile = directory.appendingPathComponent("resolutions.json")
        let recovery = CommandRecovery(gate: gate, scope: scope, file: recoveryFile, status: { id in
            #expect(id == original.id)
            return await status.read()
        })
        await recovery.recoverOnce()
        #expect(try await gate.restore().map(\.id) == [original.id])
        await status.set(503)
        await recovery.recoverOnce()
        #expect(try await gate.restore().map(\.id) == [original.id])
        await status.set(201)
        await recovery.recoverOnce()
        #expect(try await gate.restore().isEmpty)
        await recovery.stop()
        let restarted = CommandRecovery(gate: CommandGate(scope: scope, journal: FileCommandJournal(directory: journalDirectory)),
            scope: scope, file: recoveryFile, status: { _ in
                Issue.record("A persisted resolution queried transport again")
                return nil
            })
        var updates = await restarted.subscribe().makeAsyncIterator()
        let replay = try #require(await updates.next())
        #expect(replay.id == original.id)
        #expect(replay.succeeded)
        #expect(replay.status == 201)
        await restarted.stop()
    }

    @Test func failedCheckpointNeverUnblocksCommand() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let blocker = directory.appendingPathComponent("not-a-directory")
        try Data().write(to: blocker)
        let scope = CommandScope(server: URL(string: "https://example.test")!, deviceID: "client-1", credentialRevision: "revision", generation: 0)
        let gate = CommandGate(scope: scope, journal: FileCommandJournal(directory: directory.appendingPathComponent("commands")))
        let result = await gate.submitIdentified(.send("node"), itemID: "node:node", in: scope, isUnknown: { _ in true }) { _ in throw URLError(.timedOut) }
        guard case .unknown(let original) = result else { Issue.record("Expected unknown"); return }
        let recovery = CommandRecovery(gate: gate, scope: scope, file: blocker.appendingPathComponent("resolutions.json"), status: { _ in 204 })
        await recovery.recoverOnce()
        #expect(try await gate.restore().map(\.id) == [original.id])
    }
}

private actor StatusProbe {
    var value: Int?
    func set(_ value: Int) { self.value = value }
    func read() -> Int? { value }
}
