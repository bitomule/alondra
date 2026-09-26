import Foundation
import Testing
import AlondraCommands

@Suite struct CommandGateTests {
    private let a = CommandScope(server: URL(string: "https://a.example")!, deviceID: "phone", credentialRevision: "key-1", generation: 0)
    private let b = CommandScope(server: URL(string: "https://b.example")!, deviceID: "phone", credentialRevision: "key-1", generation: 1)

    private actor Counter {
        var calls = 0
        func called() { calls += 1 }
    }

    @Test func lostResponseNeverReexecutesAndIsScopedToServer() async {
        let gate = CommandGate(scope: a)
        let counter = Counter()
        let result = await gate.submit(.send("node-1"), itemID: "r1", in: a, isUnknown: { _ in true }) {
            await counter.called()
            throw URLError(.timedOut)
        }
        guard case .unknown(let command) = result else {
            Issue.record("The lost response must be an unknown outcome")
            return
        }
        #expect(command.scope == a)
        #expect(command.kind == .send("node-1"))
        #expect(await counter.calls == 1)
        #expect(await gate.uncertain == [command])

        await gate.use(b)
        await gate.use(a) // A's delayed transition must not roll the actor back.
        #expect(await gate.scope == b)
        let stale = await gate.submit(.send("node-1"), in: a, isUnknown: { _ in true }) {
            await counter.called()
        }
        guard case .obsolete = stale else { Issue.record("Old scope sent to new server"); return }
        #expect(await counter.calls == 1)
        #expect(await gate.uncertain.isEmpty)
    }

    @Test func switchingCredentialWhileRequestInFlightCannotPublishOldOutcome() async {
        let gate = CommandGate(scope: a)
        let rotated = CommandScope(server: a.server, deviceID: a.deviceID, credentialRevision: "key-2", generation: 1)
        let started = AsyncStream.makeStream(of: Void.self)
        let release = AsyncStream.makeStream(of: Void.self)
        let attempt = Task {
            await gate.submit(.newRequest, in: a, isUnknown: { _ in true }) {
                started.continuation.yield(())
                for await _ in release.stream { break }
                throw URLError(.timedOut)
            }
        }
        for await _ in started.stream { break }
        await gate.use(rotated)
        release.continuation.yield(())
        let result = await attempt.value
        guard case .obsolete = result else { Issue.record("Old outcome reached current identity"); return }
        #expect(await gate.uncertain.isEmpty)
    }

    @Test func newCredentialCannotReserveUnderOldScopeBeforeActivation() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let journal = FileCommandJournal(directory: directory)
        let gate = CommandGate(scope: a, journal: journal)
        let rotated = CommandScope(server: a.server, deviceID: a.deviceID,
            credentialRevision: "key-2", generation: a.generation + 1)
        let premature = await gate.submitIdentified(.answer(7), itemID: "q7", in: rotated,
            isUnknown: { _ in true }) { _ in
                Issue.record("New credential dispatched before its scope activated")
            }
        guard case .obsolete = premature else { Issue.record("Mismatched credential was accepted"); return }
        #expect(try await journal.records().isEmpty, "No old-credential reservation may block the new signer")

        await gate.use(rotated)
        let ready = await gate.submitIdentified(.answer(7), itemID: "q7", in: rotated,
            isUnknown: { _ in true }) { id in
                let reservation = try #require(try await journal.records().first { $0.command.id == id })
                #expect(reservation.command.scope == rotated)
                #expect(reservation.state == .pending)
            }
        guard case .confirmed = ready else { Issue.record("Activated credential could not dispatch"); return }
    }

    @Test func explicitServerFailureIsRejectedWithoutUnknownRecord() async {
        let gate = CommandGate(scope: a)
        let result = await gate.submit(.answer(7), in: a, isUnknown: { _ in false }) {
            throw URLError(.badServerResponse)
        }
        guard case .rejected = result else { Issue.record("Server refusal became uncertain"); return }
        #expect(await gate.uncertain.isEmpty)
    }
}
