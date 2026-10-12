@testable import AlondraFeed
import AlondraContract
import Foundation
import Testing

private actor StartupViewPeer: ViewTransport {
    private var epoch = "epoch-one"
    func changeEpoch() { epoch = "epoch-two" }
    func fetchView(key: String, client: ViewClient, lang: String, known: String?) async throws -> ViewFetch {
        try ContractCodec.decode(ViewFetch.self, from: Data("""
        {"protocol":1,"view":"\(key)","key":"\(key)","client":"ios","version":"loaded", "cursor":"cursor",
        "data_epoch":"\(epoch)","status":{"version":"status","summary":"Ready","badge":0,"counts":[],"gauges":[],
        "orchestrator":{"text":"idle","tone":"neutral"}},"sections":[]}
        """.utf8))
    }
    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed { throw CancellationError() }
    func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse {
        Issue.record("startup recovery must never POST an action")
        throw CancellationError()
    }
}

private actor StartupRecoveryProbe {
    let starts: AsyncStream<String>
    private let continuation: AsyncStream<String>.Continuation
    private var held: [CheckedContinuation<Void, Never>] = []
    private var released = false
    private(set) var epochs: [String] = []
    private(set) var stillCurrent: [Bool] = []
    private(set) var active = 0
    private(set) var peak = 0
    init() {
        let pair = AsyncStream<String>.makeStream()
        starts = pair.stream; continuation = pair.continuation
    }
    func run(epoch: String, isCurrent: @escaping @Sendable () async -> Bool) async {
        epochs.append(epoch); active += 1; peak = max(peak, active)
        continuation.yield(epoch)
        if !released { await withCheckedContinuation { held.append($0) } }
        stillCurrent.append(await isCurrent())
        active -= 1
    }
    func release() {
        released = true
        let waiting = held; held.removeAll()
        for waiter in waiting { waiter.resume() }
    }
}

private enum StartupRecoveryTransition: Sendable, CaseIterable { case newEpoch, stop }

@Suite struct StartupActionRecoveryTests {
    @Test func theFirstDocumentArrivesWhileItsPendingRecoveryIsStillHeld() async throws {
        let peer = StartupViewPeer()
        let recovery = StartupRecoveryProbe()
        var starts = recovery.starts.makeAsyncIterator()
        let feed = FeedSession(transport: peer, client: .ios, lang: "en", reconcileActions: { epoch, isCurrent in
            await recovery.run(epoch: epoch, isCurrent: isCurrent)
        })
        let result = try await feed.load("home")
        #expect(await starts.next() == "epoch-one")
        if case let .document(document) = result { #expect(document.dataEpoch == "epoch-one") }
        else { Issue.record("recovery cannot replace or delay the first loaded document") }
        #expect(await recovery.active == 1)
        #expect(await feed.currentSnapshot().documents["home"] != nil)
        await recovery.release()
        await feed.drainRecovery()
        #expect(await recovery.stillCurrent == [true])
        await feed.stop()
    }

    @Test(arguments: StartupRecoveryTransition.allCases)
    private func anObsoleteRecoveryKeepsItsPhysicalOwnershipAndCannotApply(_ transition: StartupRecoveryTransition) async throws {
        let peer = StartupViewPeer()
        let recovery = StartupRecoveryProbe()
        var starts = recovery.starts.makeAsyncIterator()
        let feed = FeedSession(transport: peer, client: .ios, lang: "en", reconcileActions: { epoch, isCurrent in
            await recovery.run(epoch: epoch, isCurrent: isCurrent)
        })
        _ = try await feed.load("home")
        #expect(await starts.next() == "epoch-one")
        switch transition {
        case .newEpoch:
            await peer.changeEpoch()
            _ = try await feed.load("tasks")
        case .stop: await feed.stop()
        }
        #expect(await recovery.epochs == ["epoch-one"])
        #expect(await recovery.active == 1)
        await recovery.release()
        await feed.drainRecovery()
        #expect(await recovery.peak == 1)
        if transition == .newEpoch {
            #expect(await recovery.epochs == ["epoch-one", "epoch-two"])
            #expect(await recovery.stillCurrent == [false, true])
        } else {
            #expect(await recovery.epochs == ["epoch-one"])
            #expect(await recovery.stillCurrent == [false])
        }
        #expect(await recovery.active == 0)
        await feed.stop()
    }
}
