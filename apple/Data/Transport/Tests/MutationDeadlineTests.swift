@testable import AlondraTransport
import AlondraTestSupport
import Foundation
import CryptoKit
import Testing
import Synchronization

final class MutationTestClock: Sendable {
    private struct Waiter {
        let deadline: ContinuousClock.Instant
        let continuation: CheckedContinuation<Void, any Error>
    }
    private struct State {
        var now: ContinuousClock.Instant
        var waiters: [UUID: Waiter] = [:]
        var cancellations = 0
    }
    private let state: Mutex<State>
    let timerWaits: AsyncStream<ContinuousClock.Instant>
    private let timerWaitContinuation: AsyncStream<ContinuousClock.Instant>.Continuation
    var timerCancellations: Int { state.withLock { $0.cancellations } }

    init() {
        state = Mutex(State(now: ContinuousClock().now))
        let pair = AsyncStream<ContinuousClock.Instant>.makeStream(bufferingPolicy: .bufferingNewest(1))
        timerWaits = pair.stream; timerWaitContinuation = pair.continuation
    }

    var clock: RequestClock {
        RequestClock(now: { self.state.withLock { $0.now } }, waitUntil: { try await self.wait(until: $0) })
    }

    func advance(by duration: Duration) {
        let ready = state.withLock { state in
            state.now = state.now.advanced(by: duration)
            let due = state.waiters.filter { $0.value.deadline <= state.now }
            for id in due.keys { state.waiters.removeValue(forKey: id) }
            return due.values.map(\.continuation)
        }
        for continuation in ready { continuation.resume() }
    }

    private func wait(until deadline: ContinuousClock.Instant) async throws {
        let id = UUID()
        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                let immediate: Result<Void, any Error>? = state.withLock { state in
                    if Task.isCancelled { return .failure(CancellationError()) }
                    if state.now >= deadline { return .success(()) }
                    state.waiters[id] = Waiter(deadline: deadline, continuation: continuation)
                    self.timerWaitContinuation.yield(deadline)
                    return nil
                }
                if let immediate { continuation.resume(with: immediate) }
            }
        } onCancel: {
            let waiter = self.state.withLock { state in
                guard let waiter = state.waiters.removeValue(forKey: id) else { return nil as Waiter? }
                state.cancellations += 1
                return waiter
            }
            waiter?.continuation.resume(throwing: CancellationError())
        }
    }
}

private actor DeadlineLoopbackPeer: HTTPTransport {
    private let key: P256.Signing.PublicKey
    private let clock: MutationTestClock
    private(set) var actionTimeouts: [TimeInterval] = []
    private(set) var effects = 0
    private(set) var rejectedEffects: Int?
    private var paired = false
    private let completedAction: HeldMutationWork?

    init(key: P256.Signing.PublicKey, clock: MutationTestClock, completedAction: HeldMutationWork? = nil) {
        self.key = key; self.clock = clock; self.completedAction = completedAction
    }

    func send(_ request: URLRequest) async throws -> HTTPReply {
        let path = try #require(request.url?.path)
        switch path {
        case "/pairing":
            clock.advance(by: .seconds(2))
            return HTTPReply(status: 200, body: Data(#"{"code":"fixture-invitation"}"#.utf8))
        case "/pair":
            let body = try #require(request.httpBody)
            let value = try #require(try JSONSerialization.jsonObject(with: body) as? [String: String])
            #expect(value["code"] == "fixture-invitation")
            #expect(value["public_key"] == key.x963Representation.base64EncodedString())
            clock.advance(by: .seconds(3))
            paired = true
            return HTTPReply(status: 200, body: Data(#"{"device_id":"current-phone"}"#.utf8))
        case "/actions":
            actionTimeouts.append(request.timeoutInterval)
            let signature = try #require(request.value(forHTTPHeaderField: "X-Alondra-Signature").flatMap { Data(base64Encoded: $0) })
            let timestamp = try #require(request.value(forHTTPHeaderField: "X-Alondra-Timestamp"))
            let message = LoopbackSignature.message(method: "POST", target: path, timestamp: timestamp, body: request.httpBody ?? Data())
            #expect(key.isValidSignature(try P256.Signing.ECDSASignature(derRepresentation: signature), for: message))
            if request.value(forHTTPHeaderField: "X-Alondra-Device") != "current-phone" {
                rejectedEffects = effects
                clock.advance(by: .seconds(110))
                return HTTPReply(status: 401, body: Data())
            }
            #expect(paired)
            effects += 1
            await completedAction?.perform()
            return HTTPReply(status: 200, body: Data(#"{"ok":true}"#.utf8))
        default: throw LoopbackSessionError.invalidResponse
        }
    }
}

@Suite(.temporaryFiles) struct MutationDeadlineTests {
    @Test func rePairingAfterAnUnappliedRefusalDoesNotRenewTheMutationBudget() async throws {
        let directory = try TestFiles.directory()
        let key = try P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 1, count: 32))
        let identity = LoopbackIdentityStore(directory: directory, makeKey: { .software(key) })
        _ = try await identity.signingKey()
        try await identity.save(deviceID: "retired-phone")
        let time = MutationTestClock()
        let peer = DeadlineLoopbackPeer(key: key.publicKey, clock: time)
        let session = LoopbackSession(identity: identity, deviceName: "Fixture", now: { Date(timeIntervalSince1970: 1) },
            requestClock: time.clock, transport: peer)
        let request = try APIRequest(endpoint: Endpoint<Data>.data(.post, path: ["actions"], body: Data("{}".utf8)), operationID: "loopback-budget")
        let response = try await session.perform(request)
        #expect(response == .completed(status: 200, body: Data(#"{"ok":true}"#.utf8)))
        #expect(await peer.rejectedEffects == 0, "the first401 is the identity/signature guard before any handler effect")
        #expect(await peer.effects == 1)
        #expect(await peer.actionTimeouts == [120, 5], "pairing and its read waits consume the same total mutation budget")
        await session.deactivate()
        await session.mutations.drain()
    }
}

/// A Mac's direct port that runs each request ID once and replays its sealed answer for the same
/// bytes. It can spend the test clock before answering, lose its answers, or hold them.
private actor DeadlineDirectMac: HTTPTransport {
    let macSigning = P256.Signing.PrivateKey()
    let macAgreement = P256.KeyAgreement.PrivateKey()
    let device = P256.Signing.PrivateKey()
    let deviceAgreement = P256.KeyAgreement.PrivateKey()
    private let clock: MutationTestClock
    private var journal: [String: (request: Data, response: Data)] = [:]
    private var runs: [String: Int] = [:]
    private var received: [String: [Data]] = [:]
    private(set) var requestTimeouts: [TimeInterval] = []
    private var answerTakes = Duration.zero
    private var losesAnswers = false
    private var helloGate: HeldMutationWork?
    private var answerGate: HeldMutationWork?

    init(clock: MutationTestClock) { self.clock = clock }

    nonisolated var identity: DirectMacIdentity {
        DirectMacIdentity(macID: "mac-1", name: "Studio",
                          signingKey: Base64URL.encode(macSigning.publicKey.x963Representation),
                          agreementKey: Base64URL.encode(macAgreement.publicKey.x963Representation))
    }

    func answer(after duration: Duration) { answerTakes = duration }
    func loseAnswers(_ value: Bool) { losesAnswers = value }
    func holdHello(_ gate: HeldMutationWork) { helloGate = gate }
    func holdAnswers(_ gate: HeldMutationWork) { answerGate = gate }
    func runs(of requestID: String) -> Int { runs[requestID, default: 0] }
    func sent(_ requestID: String) -> [Data] { received[requestID, default: []] }

    func send(_ request: URLRequest) async throws -> HTTPReply {
        let path = try #require(request.url?.path)
        if path == "/direct/hello" {
            await helloGate?.perform()
            return HTTPReply(status: 200, body: try JSONEncoder().encode(identity))
        }
        #expect(path == "/direct/request")
        let body = try #require(request.httpBody)
        let envelope = try JSONDecoder().decode(Envelope.self, from: body)
        received[envelope.requestID, default: []].append(body)
        requestTimeouts.append(request.timeoutInterval)
        let answer = try run(envelope, bytes: body)
        await answerGate?.perform()
        clock.advance(by: answerTakes)
        if losesAnswers { throw URLError(.networkConnectionLost) }
        guard let answer else { return HTTPReply(status: 409, body: Data(#"{"error":"idempotency_conflict"}"#.utf8)) }
        return HTTPReply(status: 200, body: answer)
    }

    private func run(_ envelope: Envelope, bytes: Data) throws -> Data? {
        if let stored = journal[envelope.requestID] { return stored.request == bytes ? stored.response : nil }
        _ = try envelope.open(senderKey: device.publicKey, recipientKey: macAgreement, sender: "phone-1",
                              recipient: "mac-1", route: "mac-1", nowMilliseconds: nil)
        runs[envelope.requestID, default: 0] += 1
        let answer = try JSONEncoder().encode(Envelope.seal(route: "mac-1", sender: "mac-1", recipient: "phone-1",
            requestID: envelope.requestID, kind: "response",
            expiresAtMilliseconds: UInt64(Date().timeIntervalSince1970 * 1000) + 60_000,
            plaintext: Data(#"{"status":200,"body":{"ok":true}}"#.utf8),
            senderKey: macSigning, recipientKey: deviceAgreement.publicKey))
        journal[envelope.requestID] = (bytes, answer)
        return answer
    }
}

private struct DeadlineDirectFixture {
    let clock: MutationTestClock
    let mac: DeadlineDirectMac
    let journal: EnvelopeJournal
    let session: DirectSession

    init(clock: MutationTestClock = MutationTestClock()) throws {
        self.clock = clock
        mac = DeadlineDirectMac(clock: clock)
        let http = DirectHTTP(transport: mac)
        let router = DirectRouter(pinned: mac.identity, saved: [DirectEndpoint(host: "192.168.1.5", port: 47810, kind: .localNetwork)],
                                  http: http)
        journal = try EnvelopeJournal(scope: EnvelopeJournalScope(relay: URL(string: "https://relay.fixture.invalid")!,
            macID: "mac-1", deviceID: "phone-1", credentialRevision: "fixture"), directory: TestFiles.directory())
        session = DirectSession(macID: "mac-1", deviceID: "phone-1", signing: mac.device, agreement: mac.deviceAgreement,
            macSigning: mac.macSigning.publicKey, macAgreement: mac.macAgreement.publicKey, router: router, http: http,
            journal: journal, pause: { _ in }, requestClock: clock.clock)
    }

    func action(_ id: String, body: Data = Data(#"{"command_id":"semantic-command"}"#.utf8)) throws -> APIRequest {
        try APIRequest(endpoint: Endpoint<Data>.data(.post, path: ["actions"], body: body), operationID: id)
    }

    func close() async {
        await session.deactivate()
        await session.mutations.drain()
    }
}

private let macAnswer = APIResponse.completed(status: 200, body: Data(#"{"ok":true}"#.utf8))

extension MutationDeadlineTests {
    @Test func aSilentMacStopsTheMutationWithinItsOriginalBudgetAndTheSameSessionResendsIt() async throws {
        let f = try DeadlineDirectFixture()
        let request = try f.action("direct-budget")
        await f.mac.answer(after: .seconds(55))
        await f.mac.loseAnswers(true)
        #expect(try await f.session.perform(request) == .unknown)
        await f.session.mutations.drain()
        #expect(await f.mac.requestTimeouts == [20, 20, 10], "every try spends the same 120-second budget")
        await f.mac.answer(after: .zero)
        await f.mac.loseAnswers(false)
        #expect(try await f.session.resend(request) == macAnswer)
        let sent = await f.mac.sent(request.operationID)
        #expect(sent.count == 4)
        #expect(Set(sent).count == 1, "recovery resends the bytes sealed for the first try")
        #expect(await f.mac.runs(of: request.operationID) == 1)
        await f.close()
    }
}

private actor HeldMutationWork {
    private var held: [CheckedContinuation<Void, Never>] = []
    private var released = false
    private(set) var starts = 0
    let startStates: AsyncStream<Int>
    private let startContinuation: AsyncStream<Int>.Continuation

    init() {
        let pair = AsyncStream<Int>.makeStream(bufferingPolicy: .bufferingNewest(1))
        startStates = pair.stream; startContinuation = pair.continuation
    }

    func perform() async {
        starts += 1
        startContinuation.yield(starts)
        if !released { await withCheckedContinuation { held.append($0) } }
    }
    func release() {
        released = true
        let callbacks = held
        held.removeAll()
        for callback in callbacks { callback.resume() }
    }
}

extension MutationDeadlineTests {
    @Test func expiredWorkersRetainTheirSlotsUntilThePhysicalWorkReturns() async throws {
        let clock = MutationTestClock()
        let runner = MutationRunner(clock: clock.clock)
        let held = HeldMutationWork()
        var states = runner.states.makeAsyncIterator()
        var starts = await held.startStates.makeAsyncIterator()
        let first = (0..<4).map { _ in Task {
            try await runner.run(unknown: { APIResponse.unknown }) {
                try MutationScope.possibleDispatch()
                await held.perform()
                return APIResponse.completed(status: 200, body: Data())
            }
        } }
        while let state = await states.next() {
            if state.waiters == 4 { break }
        }
        while let count = await starts.next() { if count == 4 { break } }
        let queued = Task {
            try await runner.run(unknown: { APIResponse.unknown }) {
                try MutationScope.possibleDispatch()
                await held.perform()
                return APIResponse.completed(status: 200, body: Data())
            }
        }
        while let state = await states.next() {
            if state.waiters == 5 {
                #expect(state.workers == 4)
                break
            }
        }
        queued.cancel()
        let queuedResult = await queued.result
        if case let .failure(error) = queuedResult { #expect(error is CancellationError) }
        else { Issue.record("a queued mutation must cancel before dispatch") }
        clock.advance(by: .seconds(120))
        for task in first { #expect(try await task.value == .unknown) }
        #expect(await runner.runningWorkers.count == 4)
        let replacement = Task {
            try await runner.run(unknown: { APIResponse.unknown }) {
                await held.perform()
                return APIResponse.completed(status: 204, body: Data())
            }
        }
        while let state = await states.next() {
            if state.waiters == 1 {
                #expect(state.workers == 4, "late workers still occupy every physical slot")
                break
            }
        }
        await held.release()
        #expect(try await replacement.value == .completed(status: 204, body: Data()))
        await runner.drain()
        #expect(await held.starts == 5, "the cancelled queued mutation never reaches physical work")
        #expect(await runner.runningWorkers.isEmpty)
        #expect(await runner.timerTask == nil)
    }

    @Test func nestedCarriersUseOneBudgetAndOneCoordinator() async throws {
        let clock = MutationTestClock()
        let outer = MutationRunner(clock: clock.clock)
        let inner = MutationRunner(clock: clock.clock)
        let result = try await outer.run(unknown: { APIResponse.unknown }) {
            clock.advance(by: .seconds(115))
            return try await inner.run(unknown: { APIResponse.unknown }) {
                #expect(try MutationScope.remaining(cap: .seconds(20)) == .seconds(5))
                #expect(await inner.runningWorkers.isEmpty)
                try MutationScope.possibleDispatch()
                return APIResponse.completed(status: 200, body: Data())
            }
        }
        #expect(result == .completed(status: 200, body: Data()))
        await outer.drain()
        await inner.drain()
        #expect(await outer.runningWorkers.isEmpty)
        #expect(await outer.timerTask == nil)
    }
}

extension MutationDeadlineTests {
    @Test(arguments: [false, true])
    func anAlreadyCancelledCallerKeepsTheExistingDispatchUncertainty(knownDispatched: Bool) async throws {
        let clock = MutationTestClock()
        let runner = MutationRunner(clock: clock.clock)
        let held = HeldMutationWork()
        var starts = await held.startStates.makeAsyncIterator()
        let caller = Task {
            await held.perform()
            return try await runner.run(knownDispatched: knownDispatched, unknown: { APIResponse.unknown }) {
                Issue.record("an already cancelled caller cannot begin physical work")
                return APIResponse.completed(status: 200, body: Data())
            }
        }
        #expect(await starts.next() == 1)
        caller.cancel()
        await held.release()
        let result = await caller.result
        if knownDispatched { #expect((try? result.get()) == .unknown) }
        else if case let .failure(error) = result { #expect(error is CancellationError) }
        else { Issue.record("a cancelled undispatched request must throw cancellation") }
        await runner.drain()
        #expect(await runner.runningWorkers.isEmpty)
        #expect(await runner.timerTask == nil)
        #expect(await runner.completionTask == nil)
    }
}

extension MutationDeadlineTests {
    @Test func aNestedCarrierKeepsKnownDispatchEvidenceWhenTheSharedBudgetExpires() async throws {
        let clock = MutationTestClock()
        let outer = MutationRunner(clock: clock.clock)
        let inner = MutationRunner(clock: clock.clock)
        let task = Task {
            try await outer.run(unknown: { APIResponse.unknown }) {
                try await inner.run(knownDispatched: true, unknown: { APIResponse.unknown }) {
                    clock.advance(by: .seconds(120))
                    try MutationScope.check()
                    return APIResponse.completed(status: 200, body: Data())
                }
            }
        }
        let result = await task.result
        #expect((try? result.get()) == .unknown)
        await outer.drain()
        await inner.drain()
    }

    @Test func anExpiredNestedCarrierNeverDelegatesPhysicalWork() async throws {
        let clock = MutationTestClock()
        let budget = MutationBudget(clock: clock.clock, duration: .seconds(1), knownDispatched: false)
        let inner = MutationRunner(clock: clock.clock)
        let effects = Mutex(0)
        clock.advance(by: .seconds(1))
        var expired = false
        do {
            _ = try await MutationScope.$current.withValue(budget) {
                try await inner.run(unknown: { APIResponse.unknown }) {
                    effects.withLock { $0 += 1 }
                    return APIResponse.completed(status: 200, body: Data())
                }
            }
        } catch RequestInterruption.expired { expired = true }
        #expect(expired)
        #expect(effects.withLock { $0 } == 0)
        await inner.drain()
    }

    @Test func aLateMutationAnswerStaysUnknownAndTheSameSessionRecoversItWithoutRunningItAgain() async throws {
        let f = try DeadlineDirectFixture()
        let request = try f.action("late-mutation")
        await f.mac.answer(after: .seconds(120))
        #expect(try await f.session.perform(request) == .unknown, "the Mac ignores the advertised HTTP timeout")
        await f.session.mutations.drain()
        await f.mac.answer(after: .zero)
        #expect(try await f.session.resend(request) == macAnswer)
        let gate = HeldMutationWork()
        var starts = await gate.startStates.makeAsyncIterator()
        let cancelled = Task {
            await gate.perform()
            return try await f.session.perform(request)
        }
        #expect(await starts.next() == 1)
        cancelled.cancel()
        await gate.release()
        #expect((try? await cancelled.value) == .unknown)
        let sent = await f.mac.sent(request.operationID)
        #expect(sent.count == 2)
        #expect(sent.first == sent.last)
        #expect(await f.mac.runs(of: request.operationID) == 1)
        let entry = try await f.journal.entry(requestID: request.operationID)
        #expect(entry?.phase == .acknowledged)
        #expect(entry?.lastResponse != nil)
        await f.close()
    }
}

extension MutationDeadlineTests {
    @Test(arguments: [false, true])
    func aCancelledDuplicateStaysUnknownWhileTheOriginalCanStillSendOrHasArrived(heldAnswer: Bool) async throws {
        let f = try DeadlineDirectFixture()
        let held = HeldMutationWork()
        if heldAnswer { await f.mac.holdAnswers(held) } else { await f.mac.holdHello(held) }
        var heldStarts = await held.startStates.makeAsyncIterator()
        let request = try f.action("shared-operation")
        let original = Task { try await f.session.perform(request) }
        #expect(await heldStarts.next() == 1)
        let gate = HeldMutationWork()
        var starts = await gate.startStates.makeAsyncIterator()
        let duplicate = Task {
            await gate.perform()
            return try await f.session.perform(request)
        }
        #expect(await starts.next() == 1)
        duplicate.cancel()
        await gate.release()
        #expect((try? await duplicate.value) == .unknown, "another owner may still send this operation ID")
        #expect(await f.mac.sent(request.operationID).count == (heldAnswer ? 1 : 0))
        await held.release()
        #expect(try await original.value == macAnswer)
        #expect(await f.mac.sent(request.operationID).count == 1)
        #expect(await f.mac.runs(of: request.operationID) == 1)
        await f.close()
    }
}

extension MutationDeadlineTests {
    @Test func dispatchEvidenceIsSharedWhileTheFirstOwnerCanStillSend() async throws {
        let f = try DeadlineDirectFixture()
        let evidence = MutationDispatchEvidence()
        let hello = HeldMutationWork()
        await f.mac.holdHello(hello)
        let firstRunner = MutationRunner(clock: f.clock.clock)
        let duplicateRunner = MutationRunner(clock: f.clock.clock)
        var starts = await hello.startStates.makeAsyncIterator()
        let request = try f.action("shared-evidence")
        let original = Task {
            try await firstRunner.run(dispatchEvidence: evidence, unknown: { APIResponse.unknown }) {
                try MutationScope.reserve(evidence)
                return try await f.session.perform(request)
            }
        }
        #expect(await starts.next() == 1)
        let gate = HeldMutationWork()
        var gates = await gate.startStates.makeAsyncIterator()
        let duplicate = Task {
            await gate.perform()
            return try await duplicateRunner.run(dispatchEvidence: evidence, unknown: { APIResponse.unknown }) {
                Issue.record("a cancelled duplicate cannot begin sealing or sending")
                return try await f.session.perform(request)
            }
        }
        #expect(await gates.next() == 1)
        duplicate.cancel()
        await gate.release()
        #expect((try? await duplicate.value) == .unknown)
        #expect(await f.mac.sent(request.operationID).isEmpty, "the first reservation is uncertainty, not evidence of a send")
        await hello.release()
        #expect(try await original.value == macAnswer)
        await firstRunner.drain()
        await duplicateRunner.drain()
        #expect(await f.mac.sent(request.operationID).count == 1)
        #expect(await f.mac.runs(of: request.operationID) == 1)
        await f.close()
    }
}

extension MutationDeadlineTests {
    @Test(arguments: [false, true])
    func onlyPossiblePOSTEvidenceSurvivesThePhysicalOwnersCompletion(possiblePOST: Bool) async throws {
        let clock = MutationTestClock()
        let evidence = MutationDispatchEvidence()
        let firstRunner = MutationRunner(clock: clock.clock)
        let nextRunner = MutationRunner(clock: clock.clock, duration: .seconds(1))
        let held = HeldMutationWork()
        var starts = await held.startStates.makeAsyncIterator()
        let first = Task {
            try await firstRunner.run(dispatchEvidence: evidence, unknown: { APIResponse.unknown }) {
                if possiblePOST { try MutationScope.possibleDispatch() }
                await held.perform()
                try MutationScope.check()
                return APIResponse.completed(status: 200, body: Data())
            }
        }
        #expect(await starts.next() == 1)
        clock.advance(by: .seconds(120))
        let firstResult = await first.result
        if possiblePOST { #expect((try? firstResult.get()) == .unknown) }
        else if case let .failure(error) = firstResult { #expect((error as? URLError)?.code == .timedOut) }
        else { Issue.record("the original owner expired without a possible POST") }
        await held.release()
        await firstRunner.drain()
        let nextGate = HeldMutationWork()
        var nextStarts = await nextGate.startStates.makeAsyncIterator()
        let effects = Mutex(0)
        let next = Task {
            try await nextRunner.run(dispatchEvidence: evidence, unknown: { APIResponse.unknown }) {
                await nextGate.perform()
                try MutationScope.check()
                effects.withLock { $0 += 1 }
                return APIResponse.completed(status: 200, body: Data())
            }
        }
        #expect(await nextStarts.next() == 1)
        clock.advance(by: .seconds(1))
        let nextResult = await next.result
        if possiblePOST { #expect((try? nextResult.get()) == .unknown) }
        else if case let .failure(error) = nextResult { #expect((error as? URLError)?.code == .timedOut) }
        else { Issue.record("a finished reservation cannot imply an effect for a future owner") }
        await nextGate.release()
        await nextRunner.drain()
        #expect(effects.withLock { $0 } == 0)
        #expect(await firstRunner.runningWorkers.isEmpty)
        #expect(await nextRunner.runningWorkers.isEmpty)
        #expect(await firstRunner.completionTask == nil)
        #expect(await nextRunner.completionTask == nil)
    }
}

extension MutationDeadlineTests {
    @Test func evidenceRemainsSharedUntilEveryWrapperAndPhysicalOwnerHasFinished() {
        let store = MutationEvidenceStore()
        let first = store.acquire("store-operation", owner: UUID())
        let second = store.acquire("store-operation", owner: UUID())
        #expect(first.evidence === second.evidence)
        store.retain(first.requestID)
        store.forget(first.requestID)
        second.evidence.release(by: second.ownerID)
        store.release(first)
        store.release(first)
        first.evidence.release(by: first.ownerID)
        let third = store.acquire(first.requestID, owner: UUID())
        #expect(third.evidence === first.evidence, "a duplicate release cannot erase the second wrapper")
        store.release(second)
        store.release(third)
        let fourth = store.acquire(first.requestID, owner: UUID())
        #expect(fourth.evidence === first.evidence, "logical release cannot erase a physical owner")
        third.evidence.release(by: third.ownerID)
        store.release(fourth)
        fourth.evidence.release(by: fourth.ownerID)
        let fresh = store.acquire(first.requestID, owner: UUID())
        #expect(fresh.evidence !== first.evidence, "an unretained entry is removed after its last physical owner")
        store.release(fresh)
        fresh.evidence.release(by: fresh.ownerID)
    }

    @Test func retainedPOSTEvidenceSurvivesEveryPriorOwnersRelease() async throws {
        let store = MutationEvidenceStore()
        let first = store.acquire("retained-operation", owner: UUID())
        let initial = MutationBudget(clock: .continuous, duration: .seconds(120), knownDispatched: false,
            dispatchEvidence: first.evidence, ownerID: first.ownerID)
        try initial.markPossibleDispatch()
        store.retain(first.requestID)
        store.release(first)
        initial.finishPhysicalWork()
        let next = store.acquire(first.requestID, owner: UUID())
        #expect(next.evidence === first.evidence)
        let clock = MutationTestClock()
        let runner = MutationRunner(clock: clock.clock, duration: .seconds(1))
        let held = HeldMutationWork()
        var starts = await held.startStates.makeAsyncIterator()
        let call = Task {
            try await runner.run(dispatchEvidence: next.evidence, ownerID: next.ownerID, unknown: { APIResponse.unknown }) {
                await held.perform()
                try MutationScope.check()
                return APIResponse.completed(status: 200, body: Data())
            }
        }
        #expect(await starts.next() == 1)
        clock.advance(by: .seconds(1))
        #expect(try await call.value == .unknown)
        await held.release()
        await runner.drain()
        store.release(next)
        store.forget(first.requestID)
        let fresh = store.acquire(first.requestID, owner: UUID())
        #expect(fresh.evidence !== first.evidence)
        store.release(fresh)
        fresh.evidence.release(by: fresh.ownerID)
    }
}

extension MutationDeadlineTests {
    @Test func aLateLoopbackAnswerUpdatesTheSessionAfterTheCallerHasExpired() async throws {
        let key = try P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 1, count: 32))
        let identity = LoopbackIdentityStore(directory: try TestFiles.directory(), makeKey: { .software(key) })
        _ = try await identity.signingKey()
        try await identity.save(deviceID: "retired-phone")
        let clock = MutationTestClock()
        let held = HeldMutationWork()
        let peer = DeadlineLoopbackPeer(key: key.publicKey, clock: clock, completedAction: held)
        let session = LoopbackSession(identity: identity, deviceName: "Fixture", now: { Date(timeIntervalSince1970: 1) },
            requestClock: clock.clock, transport: peer)
        let runner = MutationRunner(clock: clock.clock)
        var starts = await held.startStates.makeAsyncIterator()
        let request = try APIRequest(endpoint: Endpoint<Data>.data(.post, path: ["actions"], body: Data("{}".utf8)), operationID: "late-loopback")
        let call = Task {
            try await runner.run(knownDispatched: true, unknown: { APIResponse.unknown }) { try await session.perform(request) }
        }
        #expect(await starts.next() == 1)
        #expect(await peer.effects == 1)
        clock.advance(by: .seconds(5))
        #expect(try await call.value == .unknown)
        #expect(await runner.runningWorkers.count == 1)
        await held.release()
        await runner.drain()
        #expect(await session.outcome(requestID: request.operationID) == .completed(status: 200, body: Data(#"{"ok":true}"#.utf8)))
        #expect(await peer.effects == 1)
        await session.deactivate()
        await session.mutations.drain()
    }

    @Test(arguments: [false, true])
    func aLateAnswerAfterItsSendOrResendCallerExpiredIsRecoveredByTheSameBytes(resend: Bool) async throws {
        let f = try DeadlineDirectFixture()
        let request = try f.action("late-answer")
        if resend {
            await f.mac.loseAnswers(true)
            #expect(try await f.session.perform(request) == .unknown)
            await f.session.mutations.drain()
            await f.mac.loseAnswers(false)
        }
        let held = HeldMutationWork()
        await f.mac.holdAnswers(held)
        var starts = await held.startStates.makeAsyncIterator()
        let runner = MutationRunner(clock: f.clock.clock)
        let call = Task {
            try await runner.run(unknown: { APIResponse.unknown }) {
                if resend { return try await f.session.resend(request) }
                return try await f.session.perform(request)
            }
        }
        #expect(await starts.next() == 1)
        #expect(await f.mac.runs(of: request.operationID) == 1)
        f.clock.advance(by: .seconds(120))
        #expect(try await call.value == .unknown)
        #expect(await runner.runningWorkers.count == 1)
        await held.release()
        await runner.drain()
        #expect(try await f.session.resend(request) == macAnswer)
        #expect(Set(await f.mac.sent(request.operationID)).count == 1)
        #expect(await f.mac.runs(of: request.operationID) == 1)
        await f.close()
    }

    @Test func timeSpentBeforeAdmissionIsAlreadyPartOfTheMutationDeadline() async throws {
        let clock = MutationTestClock()
        let startedAt = clock.clock.now()
        clock.advance(by: .seconds(121))
        let runner = MutationRunner(clock: clock.clock)
        var timedOut = false
        do {
            _ = try await runner.run(startedAt: startedAt, unknown: { APIResponse.unknown }) {
                Issue.record("an expired admission cannot begin physical preparation")
                return APIResponse.completed(status: 200, body: Data())
            }
        } catch let error as URLError { timedOut = error.code == .timedOut }
        #expect(timedOut)
        await runner.drain()
        #expect(await runner.runningWorkers.isEmpty)
        #expect(await runner.timerTask == nil)
    }
}

extension MutationDeadlineTests {
    @Test(arguments: [false, true])
    func invalidMutationsLeaveNoRecordOrRetainedEvidence(oversized: Bool) async throws {
        let f = try DeadlineDirectFixture()
        let request = oversized
            ? try f.action("invalid-envelope", body: JSONSerialization.data(withJSONObject:
                ["command_id": "semantic-command", "padding": String(repeating: "x", count: 300_000)]))
            : try f.action("read-invalid-envelope")
        await #expect(throws: SealedRequestError.invalidRequest) {
            try await f.session.perform(request)
        }
        await f.session.mutations.drain()
        #expect(try await f.journal.entry(requestID: request.operationID) == nil)
        #expect(await f.session.sealedEnvelope(for: request.operationID) == nil)
        #expect(await f.mac.sent(request.operationID).isEmpty)
        #expect(await f.mac.runs(of: request.operationID) == 0)
        await f.close()
    }
}

extension MutationDeadlineTests {
    @Test func cancellingAResendFinishesTheCallerAndKeepsTheHeldSendOwned() async throws {
        let f = try DeadlineDirectFixture()
        let request = try f.action("cancelled-resend")
        await f.mac.loseAnswers(true)
        #expect(try await f.session.perform(request) == .unknown)
        await f.session.mutations.drain()
        await f.mac.loseAnswers(false)
        let held = HeldMutationWork()
        await f.mac.holdAnswers(held)
        var starts = await held.startStates.makeAsyncIterator()
        let call = Task { try await f.session.resend(request) }
        #expect(await starts.next() == 1)
        call.cancel()
        #expect(try await call.value == .unknown)
        #expect(await f.session.mutations.runningWorkers.count == 1)
        await held.release()
        await f.session.mutations.drain()
        #expect(try await f.session.resend(request) == macAnswer)
        #expect(Set(await f.mac.sent(request.operationID)).count == 1)
        #expect(await f.mac.runs(of: request.operationID) == 1)
        await f.close()
    }
}

extension MutationDeadlineTests {
    @Test func anEarlierIncomingDeadlineReplacesTheOwnedLaterTimer() async throws {
        let clock = MutationTestClock()
        let runner = MutationRunner(clock: clock.clock)
        let held = HeldMutationWork()
        var starts = await held.startStates.makeAsyncIterator()
        var timers = clock.timerWaits.makeAsyncIterator()
        let first = Task {
            try await runner.run(unknown: { APIResponse.unknown }) {
                await held.perform()
                return APIResponse.completed(status: 200, body: Data())
            }
        }
        #expect(await starts.next() == 1)
        let originalDeadline = try #require(await timers.next())
        #expect(clock.clock.now().duration(to: originalDeadline) == .seconds(120))
        let earlierStart = clock.clock.now().advanced(by: .seconds(-115))
        let second = Task {
            try await runner.run(startedAt: earlierStart, unknown: { APIResponse.unknown }) {
                await held.perform()
                return APIResponse.completed(status: 200, body: Data())
            }
        }
        #expect(await starts.next() == 2)
        #expect(await runner.runningWorkers.count == 2)
        let retimed = clock.timerCancellations > 0
        #expect(retimed, "an earlier admitted deadline must cancel the existing later sleep")
        clock.advance(by: .seconds(5))
        var early: Result<APIResponse, any Error>?
        if retimed { early = await second.result }
        #expect(early != nil, "the caller must finish while both physical workers are still held")
        #expect(await runner.runningWorkers.count == 2)
        await held.release()
        let secondResult = await second.result
        if case let .failure(error) = secondResult { #expect((error as? URLError)?.code == .timedOut) }
        else { Issue.record("the earlier mutation budget expired before release") }
        #expect(try await first.value == .completed(status: 200, body: Data()))
        await runner.drain()
        #expect(await runner.runningWorkers.isEmpty)
        #expect(await runner.timerTask == nil)
        #expect(await runner.completionTask == nil)
    }
}

extension MutationDeadlineTests {
    @Test func cancellingTheEarliestCallerRetimesTheOwnedTimerWhileItsWorkerDrains() async throws {
        let clock = MutationTestClock()
        let runner = MutationRunner(clock: clock.clock)
        let held = HeldMutationWork()
        var starts = await held.startStates.makeAsyncIterator()
        var timers = clock.timerWaits.makeAsyncIterator()
        let first = Task {
            try await runner.run(unknown: { APIResponse.unknown }) {
                await held.perform()
                return APIResponse.completed(status: 200, body: Data())
            }
        }
        #expect(await starts.next() == 1)
        let firstDeadline = try #require(await timers.next())
        #expect(clock.clock.now().duration(to: firstDeadline) == .seconds(120))
        let earlyStart = clock.clock.now().advanced(by: .seconds(-110))
        let earliest = Task {
            try await runner.run(startedAt: earlyStart, unknown: { APIResponse.unknown }) {
                await held.perform()
                return APIResponse.completed(status: 200, body: Data())
            }
        }
        #expect(await starts.next() == 2)
        #expect(await runner.runningWorkers.count == 2)
        let retimed = clock.timerCancellations > 0
        #expect(retimed)
        if !retimed {
            await held.release()
            _ = await first.result
            _ = await earliest.result
            await runner.drain()
            return
        }
        let earlyDeadline = try #require(await timers.next())
        #expect(clock.clock.now().duration(to: earlyDeadline) == .seconds(10))
        let laterStart = clock.clock.now().advanced(by: .seconds(-10))
        let later = Task {
            try await runner.run(startedAt: laterStart, unknown: { APIResponse.unknown }) {
                await held.perform()
                return APIResponse.completed(status: 200, body: Data())
            }
        }
        #expect(await starts.next() == 3)
        #expect(await runner.runningWorkers.count == 3)
        earliest.cancel()
        let cancelled = await earliest.result
        if case let .failure(error) = cancelled { #expect(error is CancellationError) }
        else { Issue.record("an undispatched cancelled caller must report cancellation") }
        let laterDeadline = try #require(await timers.next())
        #expect(clock.clock.now().duration(to: laterDeadline) == .seconds(110))
        #expect(await runner.runningWorkers.count == 3)
        clock.advance(by: .seconds(110))
        let expired = await later.result
        if case let .failure(error) = expired { #expect((error as? URLError)?.code == .timedOut) }
        else { Issue.record("the later caller must expire before releasing physical work") }
        #expect(await runner.runningWorkers.count == 3)
        await held.release()
        #expect(try await first.value == .completed(status: 200, body: Data()))
        await runner.drain()
        #expect(await runner.runningWorkers.isEmpty)
        #expect(await runner.timerTask == nil)
        #expect(await runner.completionTask == nil)
    }
}

extension MutationDeadlineTests {
    @Test func rejectingAStoreBeyondTheTrackedLimitReleasesItsUnusedReservation() async throws {
        let clock = MutationTestClock()
        let root = MutationRunner(clock: clock.clock)
        var fixtures: [DeadlineDirectFixture] = []
        for _ in 0..<5 { fixtures.append(try DeadlineDirectFixture(clock: clock)) }
        let prepared = fixtures
        var rejected = false
        do {
            _ = try await root.run(unknown: { APIResponse.unknown }) {
                for (index, fixture) in prepared.enumerated() {
                    _ = try await fixture.session.perform(fixture.action("tracked-limit-\(index)"))
                }
                return APIResponse.completed(status: 200, body: Data())
            }
        } catch SealedRequestError.invalidRequest { rejected = true }
        await root.drain()
        #expect(rejected)
        for (index, fixture) in fixtures.prefix(4).enumerated() {
            #expect(await fixture.mac.runs(of: "tracked-limit-\(index)") == 1)
        }
        let last = fixtures[4]
        let request = try last.action("tracked-limit-4")
        #expect(await last.mac.sent(request.operationID).isEmpty)
        #expect(await last.session.sealedEnvelope(for: request.operationID) == nil)
        #expect(try await last.journal.entry(requestID: request.operationID) == nil)
        let gate = HeldMutationWork()
        var starts = await gate.startStates.makeAsyncIterator()
        let retry = Task {
            await gate.perform()
            return try await last.session.perform(request)
        }
        #expect(await starts.next() == 1)
        retry.cancel()
        await gate.release()
        if case let .failure(error) = await retry.result { #expect(error is CancellationError) }
        else { Issue.record("a released reservation leaves no owner that could still send the operation") }
        for fixture in fixtures { await fixture.close() }
    }
}
