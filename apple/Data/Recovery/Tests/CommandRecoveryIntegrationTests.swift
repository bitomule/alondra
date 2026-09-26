import AlondraRecovery
import AlondraCommands
import Foundation
import Testing

@Suite struct CommandRecoveryIntegrationTests {
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
