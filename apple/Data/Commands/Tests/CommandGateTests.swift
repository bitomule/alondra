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

    @Test func explicitServerFailureIsRejectedWithoutUnknownRecord() async {
        let gate = CommandGate(scope: a)
        let result = await gate.submit(.answer(7), in: a, isUnknown: { _ in false }) {
            throw URLError(.badServerResponse)
        }
        guard case .rejected = result else { Issue.record("Server refusal became uncertain"); return }
        #expect(await gate.uncertain.isEmpty)
    }
}
