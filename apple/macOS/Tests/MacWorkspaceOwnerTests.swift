import AlondraAccount
import AlondraTransport
import Foundation
import Testing

@MainActor
struct MacWorkspaceOwnerTests {
    @Test
    func localDaemonIsUsedWithoutTouchingTheAccount() async throws {
        let remoteStarts = Recorder()
        let owner = try makeOwner(probes: [true], remoteStarts: remoteStarts)

        await owner.run()

        #expect(owner.route == .local)
        #expect(owner.services != nil)
        #expect(remoteStarts.count == 0)
    }

    @Test
    func withoutALocalDaemonTheAccountIsRestoredOnce() async throws {
        let remoteStarts = Recorder()
        let owner = try makeOwner(probes: [false], remoteStarts: remoteStarts)

        let run = Task { await owner.run() }
        while owner.route == .searching { await Task.yield() }
        try await Task.sleep(for: .milliseconds(50))
        run.cancel()
        await run.value

        #expect(owner.route == .remote)
        #expect(owner.services == nil)
        #expect(remoteStarts.count == 1)
    }

    @Test
    func aDaemonThatStartsLaterTakesOverFromTheRemoteRoute() async throws {
        let remoteStarts = Recorder()
        let owner = try makeOwner(probes: [false, false, true], remoteStarts: remoteStarts)

        let run = Task { await owner.run() }
        for _ in 0..<200 where owner.route != .local { try await Task.sleep(for: .milliseconds(10)) }
        run.cancel()
        await run.value

        #expect(owner.route == .local)
        #expect(owner.services != nil)
        #expect(remoteStarts.count == 1)
    }

    private func makeOwner(probes: [Bool], remoteStarts: Recorder) throws -> MacWorkspaceOwner {
        let directory = FileManager.default.temporaryDirectory.appending(path: "mac-owner-\(UUID().uuidString)")
        let account = AccountSession(auth: AuthSession(browser: UnusedBrowser()), displayName: "Mac", storageDirectory: directory)
        let identity = LoopbackIdentityStore(directory: directory.appending(path: "identity"), makeKey: { .software(.init()) })
        let answers = ProbeAnswers(probes)
        return MacWorkspaceOwner(account: account, isDaemonRunning: { answers.next() },
            openLocal: {
                try await Connection.local(LoopbackSession(identity: identity, deviceName: "Mac"), identity: identity)
            },
            startRemote: { remoteStarts.record() }, reprobeInterval: .milliseconds(5))
    }
}

@MainActor
private final class Recorder {
    private(set) var count = 0
    func record() { count += 1 }
}

private final class ProbeAnswers: @unchecked Sendable {
    private let lock = NSLock()
    private var answers: [Bool]
    init(_ answers: [Bool]) { self.answers = answers }
    func next() -> Bool { lock.withLock { answers.count > 1 ? answers.removeFirst() : answers[0] } }
}

private struct UnusedBrowser: AuthBrowser {
    func authenticate(url: URL, callbackScheme: String, generation: Int) async throws -> URL { throw CancellationError() }
    func cancelAuthentication(generation: Int) async {}
}
