@testable import AlondraTransport
import CryptoKit
import Foundation
import Testing

private actor BootstrapPeer: HTTPTransport {
    nonisolated let starts: AsyncStream<Int>
    private let arrival: AsyncStream<Int>.Continuation
    private var held: [Int: CheckedContinuation<HTTPReply, Never>] = [:]
    private(set) var bootstrapCalls = 0
    private(set) var timeouts: [TimeInterval] = []
    private(set) var active = 0
    private(set) var peak = 0
    private(set) var refusals = 0
    func refused() { refusals += 1 }
    private let cooperative: Bool
    private(set) var inheritedBudget = false
    private var epoch = Data(repeating: 7, count: 16)
    private var refuseNext = false
    private var immediate = false
    init(cooperative: Bool = false) { self.cooperative = cooperative; (starts, arrival) = AsyncStream.makeStream() }
    func restart() { epoch = Data(repeating: 8, count: 16) }
    func rejectNextSigned() { refuseNext = true }
    func allowImmediateResponses() { immediate = true }
    func releaseAll(status: Int = 200, oldEpoch: Bool = false) {
        let pending = held
        held.removeAll()
        let value = oldEpoch ? Data(repeating: 7, count: 16) : epoch
        for continuation in pending.values {
            continuation.resume(returning: HTTPReply(status: status, body: Data("{\"protocol\":1,\"epoch\":\"\(Base64URL.encode(value))\"}".utf8)))
        }
    }
    func send(_ request: URLRequest) async throws -> HTTPReply {
        let url = try #require(request.url)
        if url.path == "/v1/bootstrap" {
            inheritedBudget = inheritedBudget || MutationScope.current != nil
            bootstrapCalls += 1; active += 1; peak = max(peak, active)
            timeouts.append(request.timeoutInterval)
            let id = bootstrapCalls
            if immediate {
                active -= 1
                return HTTPReply(status: 200, body: Data("{\"protocol\":1,\"epoch\":\"\(Base64URL.encode(epoch))\"}".utf8))
            }
            let response = await withCheckedContinuation { held[id] = $0; arrival.yield(id) }
            active -= 1
            if cooperative { try Task.checkCancellation() }
            return response
        }
        if refuseNext { refuseNext = false; return HTTPReply(status: 401, body: Data()) }
        guard request.value(forHTTPHeaderField: "x-relay-epoch") == Base64URL.encode(epoch) else {
            return HTTPReply(status: 401, body: Data())
        }
        return HTTPReply(status: 200, body: Data("{\"request_id\":\"\(url.lastPathComponent)\",\"status\":\"completed\",\"revision\":1}".utf8))
    }
}

@Suite(.serialized)
struct RelayBootstrapTests {
    private func transport(_ peer: BootstrapPeer) throws -> RelayTransport {
        let signing = try P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 3, count: 32))
        return try RelayTransport(base: URL(string: "https://relay.fixture.invalid")!, deviceID: "phone",
            signer: { try signing.signature(for: $0).rawRepresentation }, bearer: { "fixture" }, bearerRefused: { await peer.refused() }, transport: peer, now: { 1_000 })
    }
    private func counts(_ transport: RelayTransport) async -> AsyncStream<Int> {
        let (stream, continuation) = AsyncStream<Int>.makeStream(bufferingPolicy: .bufferingNewest(1))
        await transport.observeBootstrapWaiters { continuation.yield($0) }
        return stream
    }
    private func waitFor(_ wanted: Int, in iterator: inout AsyncStream<Int>.Iterator) async {
        while let count = await iterator.next() { if count == wanted { return } }
        Issue.record("bootstrap observer ended before its boundary")
    }
    private func seed(_ transport: RelayTransport, _ peer: BootstrapPeer) async throws {
        var starts = peer.starts.makeAsyncIterator()
        let seeded = Task { try await transport.bootstrap() }
        _ = await starts.next()
        await peer.releaseAll()
        _ = try await seeded.value
    }

    @Test func simultaneousColdCallersUseOneHTTPAndWarmReadsNeedNoBootstrap() async throws {
        let peer = BootstrapPeer(), transport = try transport(peer)
        var counts = await counts(transport).makeAsyncIterator()
        var starts = peer.starts.makeAsyncIterator()
        let calls = await Self.beginFour(in: transport)
        await waitFor(4, in: &counts)
        _ = await starts.next()
        await peer.allowImmediateResponses()
        await peer.releaseAll()
        for call in calls { #expect(try await call.value.epoch == Base64URL.encode(Data(repeating: 7, count: 16))) }
        #expect(await peer.bootstrapCalls == 1)
        #expect(await peer.peak == 1)
        for _ in 0..<4 { _ = try await transport.bootstrapIfNeeded() }
        #expect(await peer.bootstrapCalls == 1)
        await transport.observeBootstrapWaiters(nil)
    }

    @Test func cancellingOneCallerDoesNotCancelItsSharedPeer() async throws {
        let peer = BootstrapPeer(cooperative: true), transport = try transport(peer)
        var counts = await counts(transport).makeAsyncIterator()
        var starts = peer.starts.makeAsyncIterator()
        let first = Task { try await transport.bootstrap() }
        let second = Task { try await transport.bootstrap() }
        await waitFor(2, in: &counts); _ = await starts.next()
        first.cancel()
        await #expect(throws: CancellationError.self) { try await first.value }
        await waitFor(1, in: &counts)
        #expect(await peer.active == 1)
        await peer.releaseAll()
        _ = try await second.value
        #expect(await peer.bootstrapCalls == 1)
        await transport.observeBootstrapWaiters(nil)
    }

    @Test func anEndedFlightDrainsBeforeReplacementAndCannotPublishItsOldAnswer() async throws {
        let peer = BootstrapPeer(), transport = try transport(peer)
        var counts = await counts(transport).makeAsyncIterator()
        var starts = peer.starts.makeAsyncIterator()
        let old = Task { try await transport.bootstrap() }
        _ = await starts.next()
        let physical = try #require(await transport.bootstrapHTTPTask)
        old.cancel()
        await #expect(throws: CancellationError.self) { try await old.value }
        let fresh = Task { try await transport.bootstrap() }
        await waitFor(1, in: &counts)
        await peer.restart()
        #expect(await peer.bootstrapCalls == 1, "noncooperative old HTTP retains its physical slot")
        await peer.releaseAll(oldEpoch: true)
        await physical.value
        let replacement = await transport.bootstrapHTTPTask
        #expect(replacement != nil, "the retired physical response must leave a replacement for the current waiter")
        if replacement != nil { _ = await starts.next(); await peer.releaseAll() }
        #expect(try await fresh.value.epoch == Base64URL.encode(Data(repeating: 8, count: 16)))
        #expect(try await transport.signedRead("detail") == "detail")
        #expect(await peer.bootstrapCalls == 2)
        #expect(await peer.peak == 1)
        await transport.observeBootstrapWaiters(nil)
    }

    @Test(arguments: [false, true])
    func freshSameEpochBootstrapResetsOldProofButPreservesAProofReceivedDuringItsHTTP(provenDuringBootstrap: Bool) async throws {
        let peer = BootstrapPeer(), transport = try transport(peer)
        try await seed(transport, peer)
        _ = try await transport.signedRead("old-proof")
        var starts = peer.starts.makeAsyncIterator()
        let refresh = Task { try await transport.bootstrap() }
        _ = await starts.next()
        if provenDuringBootstrap { _ = try await transport.signedRead("concurrent-proof") }
        await peer.releaseAll()
        _ = try await refresh.value
        await peer.rejectNextSigned()
        await #expect(throws: RelayWireError.unauthorized) { try await transport.signedRead("refused") }
        #expect(await peer.refusals == (provenDuringBootstrap ? 0 : 1))
    }

    @Test func aRefusalInvalidatesAConcurrentOldBootstrapWithoutOverwritingTheNewEpoch() async throws {
        let peer = BootstrapPeer(), transport = try transport(peer)
        try await seed(transport, peer)
        _ = try await transport.signedRead("proven")
        var starts = peer.starts.makeAsyncIterator()
        let refresh = Task { try await transport.bootstrap() }
        _ = await starts.next()
        let physical = try #require(await transport.bootstrapHTTPTask)
        await peer.restart()
        await #expect(throws: RelayWireError.unauthorized) { try await transport.signedRead("old-epoch") }
        await peer.releaseAll(oldEpoch: true)
        await physical.value
        let replacement = await transport.bootstrapHTTPTask
        #expect(replacement != nil, "invalidation must request a current epoch rather than publish the old HTTP answer")
        if replacement != nil { _ = await starts.next(); await peer.releaseAll() }
        #expect(try await refresh.value.epoch == Base64URL.encode(Data(repeating: 8, count: 16)))
        #expect(try await transport.signedRead("new-epoch") == "new-epoch")
        #expect(await peer.bootstrapCalls == 3)
        #expect(await peer.peak == 1)
    }

    @Test func aFailedSharedBootstrapCanBeTriedAgainWithoutAStaleCache() async throws {
        let peer = BootstrapPeer(), transport = try transport(peer)
        var counts = await counts(transport).makeAsyncIterator()
        var starts = peer.starts.makeAsyncIterator()
        let calls = await Self.beginFour(in: transport)
        await waitFor(4, in: &counts); _ = await starts.next()
        await peer.releaseAll(status: 503)
        for call in calls { await #expect(throws: RelayWireError.server(503)) { try await call.value } }
        let recovered = Task { try await transport.bootstrapIfNeeded() }
        _ = await starts.next(); await peer.releaseAll()
        _ = try await recovered.value
        #expect(await peer.bootstrapCalls == 2)
        await transport.observeBootstrapWaiters(nil)
    }

    @Test func aShortMutationBudgetExpiresOnlyItsWaiterAndDoesNotShortenTheOwnedHTTP() async throws {
        let peer = BootstrapPeer(cooperative: true), transport = try transport(peer)
        let clock = MutationTestClock(), runner = MutationRunner(clock: clock.clock, duration: .seconds(1))
        var counts = await counts(transport).makeAsyncIterator()
        var starts = peer.starts.makeAsyncIterator()
        var timers = clock.timerWaits.makeAsyncIterator()
        let short = Task { try await runner.run(unknown: { false }) { _ = try await transport.bootstrap(); return true } }
        await waitFor(1, in: &counts)
        let ordinary = Task { try await transport.bootstrap() }
        await waitFor(2, in: &counts); _ = await starts.next(); _ = await timers.next()
        clock.advance(by: .seconds(1))
        await #expect(throws: URLError(.timedOut)) { try await short.value }
        await waitFor(1, in: &counts)
        #expect(await peer.active == 1)
        #expect(await peer.timeouts == [15])
        #expect(await !peer.inheritedBudget)
        await peer.releaseAll()
        _ = try await ordinary.value
        await runner.drain()
        #expect(await peer.bootstrapCalls == 1)
        await transport.observeBootstrapWaiters(nil)
    }

    private static func beginFour(in transport: isolated RelayTransport) -> [Task<RelayBootstrap, any Error>] {
        var calls: [Task<RelayBootstrap, any Error>] = []
        for _ in 0..<4 { calls.append(Task.immediate { try await transport.bootstrapIfNeeded() }) }
        return calls
    }
}
