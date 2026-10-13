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

private actor DeadlineRelayPeer: HTTPTransport {
    let phoneSigning = P256.Signing.PrivateKey()
    let phoneAgreement = P256.KeyAgreement.PrivateKey()
    let macSigning = P256.Signing.PrivateKey()
    let macAgreement = P256.KeyAgreement.PrivateKey()
    let clock: MutationTestClock
    private var journal: [String: Envelope] = [:]
    private var commandIDs: Set<String> = []
    private(set) var postIDs: [String] = []
    private(set) var effects = 0
    private(set) var posts = 0
    private(set) var statusTimeouts: [TimeInterval] = []
    private(set) var bootstrapTimeouts: [TimeInterval] = []
    private(set) var submissionTimeouts: [TimeInterval] = []
    private var completed: Bool
    private let postDuration: Duration
    private let postGate: HeldMutationWork?
    private let statusGate: HeldMutationWork?

    init(clock: MutationTestClock, postDuration: Duration = .seconds(14), completed: Bool = false, postGate: HeldMutationWork? = nil, statusGate: HeldMutationWork? = nil) {
        self.clock = clock; self.postDuration = postDuration; self.completed = completed; self.postGate = postGate; self.statusGate = statusGate
    }
    func complete() { completed = true }

    func send(_ request: URLRequest) async throws -> HTTPReply {
        let path = try #require(request.url?.path)
        if path == "/v1/bootstrap" {
            bootstrapTimeouts.append(request.timeoutInterval)
            clock.advance(by: .seconds(12))
            return HTTPReply(status: 200, body: Data("{\"protocol\":1,\"epoch\":\"\(Base64URL.encode(Data(repeating: 3, count: 16)))\"}".utf8))
        }
        let id: String
        if request.httpMethod == "POST" {
            #expect(path == "/v1/requests")
            posts += 1
            submissionTimeouts.append(request.timeoutInterval)
            let envelope = try JSONDecoder().decode(Envelope.self, from: #require(request.httpBody))
            id = envelope.requestID
            postIDs.append(id)
            let plain = try envelope.open(senderKey: phoneSigning.publicKey, recipientKey: macAgreement,
                sender: "phone-1", recipient: "mac-1", route: "mac-1", nowMilliseconds: 1_000)
            let payload = try #require(try JSONSerialization.jsonObject(with: plain) as? [String: Any])
            #expect(payload["path"] as? String == "/actions")
            let body = try #require(payload["body"] as? [String: String])
            #expect(body["command_id"] == "semantic-command")
            if journal[id] == nil {
                if commandIDs.insert(try #require(body["command_id"])).inserted { effects += 1 }
                journal[id] = try Envelope.seal(route: "mac-1", sender: "mac-1", recipient: "phone-1",
                    requestID: id, kind: "response", expiresAtMilliseconds: 121_000,
                    plaintext: Data(#"{"status":200,"body":{"ok":true}}"#.utf8),
                    senderKey: macSigning, recipientKey: phoneAgreement.publicKey)
            }
            await postGate?.perform()
            clock.advance(by: postDuration)
        } else {
            #expect(path.hasPrefix("/v1/requests/"))
            id = String(path.dropFirst("/v1/requests/".count))
            statusTimeouts.append(request.timeoutInterval)
            await statusGate?.perform()
            if !completed { clock.advance(by: .seconds(min(14, request.timeoutInterval))) }
        }
        if request.httpMethod == "GET", journal[id] == nil { return HTTPReply(status: 404, body: Data()) }
        var reply: [String: Any] = ["request_id": id, "status": completed ? "completed" : "received", "revision": completed ? 2 : 1]
        if completed { reply["response"] = try JSONSerialization.jsonObject(with: JSONEncoder().encode(#require(journal[id]))) }
        return HTTPReply(status: 200, body: try JSONSerialization.data(withJSONObject: reply, options: [.sortedKeys]))
    }

    func session(bearer: @escaping @Sendable () async throws -> String = { "fixture-token" }) throws -> RelaySession {
        try RelaySession(scope: RelayOutboxScope(relay: URL(string: "https://relay.example.test")!,
                macID: "mac-1", deviceID: "phone-1", credentialRevision: "fixture"),
            bearer: bearer, transport: self, signing: phoneSigning, agreement: phoneAgreement,
            macSigning: macSigning.publicKey, macAgreement: macAgreement.publicKey, now: { 1_000 },
            sleep: { _ in self.clock.advance(by: .seconds(1)) },
            waitForReadDeadline: clock.clock.waitUntil, requestClock: clock.clock)
    }
}

extension MutationDeadlineTests {
    @Test func relayPollingStopsWithinTheOriginalBudgetAndRecoversByGETInTheSameSession() async throws {
        let clock = MutationTestClock()
        let peer = DeadlineRelayPeer(clock: clock)
        let session = try await peer.session()
        let result = try await session.execute(requestID: "relay-budget", method: "POST", path: "/actions",
            body: Data(#"{"command_id":"semantic-command"}"#.utf8))
        #expect(result == .unknown)
        let pollingTimeouts = await peer.statusTimeouts
        #expect(pollingTimeouts == [15, 15, 15, 15, 15, 15, 4])
        #expect(await peer.bootstrapTimeouts == [15])
        #expect(await peer.submissionTimeouts == [15])
        #expect(await peer.effects == 1)
        #expect(await peer.posts == 1)
        await peer.complete()
        #expect(try await session.recoveryStatus(requestID: "relay-budget") == .completed(status: 200, body: Data(#"{"ok":true}"#.utf8)))
        try await session.retireAppliedResponse(requestID: "relay-budget")
        await #expect(throws: RelayWireError.invalidResponse) {
            try await session.execute(requestID: "relay-budget", method: "POST", path: "/actions",
                body: Data(#"{"command_id":"semantic-command"}"#.utf8))
        }
        #expect(await peer.posts == 1)
        #expect(await peer.effects == 1)
        await session.deactivate()
        await session.mutations.drain()
    }

    @Test func expirationDuringCredentialsDoesNotSubmitAMutation() async throws {
        let clock = MutationTestClock()
        let peer = DeadlineRelayPeer(clock: clock)
        let session = try await peer.session(bearer: { clock.advance(by: .seconds(121)); return "fixture-token" })
        var timedOut = false
        do {
            _ = try await session.execute(requestID: "expired-credentials", method: "POST", path: "/actions",
                body: Data(#"{"command_id":"semantic-command"}"#.utf8))
        } catch let error as URLError { timedOut = error.code == .timedOut }
        #expect(timedOut)
        #expect(await peer.posts == 0)
        #expect(await peer.effects == 0)
        await session.deactivate()
        await session.mutations.drain()
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

private actor SharedDeadlineCredentials {
    let held = HeldMutationWork()
    let states: AsyncStream<Int>
    private let continuation: AsyncStream<Int>.Continuation
    private var calls = 0
    private var task: Task<String, any Error>?
    private(set) var inheritedBudget = false

    init() {
        let pair = AsyncStream<Int>.makeStream(bufferingPolicy: .bufferingNewest(1))
        states = pair.stream; continuation = pair.continuation
    }
    func token() async throws -> String {
        calls += 1
        continuation.yield(calls)
        if let task { return try await task.value }
        inheritedBudget = MutationScope.current != nil
        let task = Task {
            await self.held.perform()
            try MutationScope.check()
            return "fixture-token"
        }
        self.task = task
        return try await task.value
    }
    func releaseAndDrain() async {
        await held.release()
        _ = await task?.result
        task = nil
    }
}

extension MutationDeadlineTests {
    @Test(arguments: [false, true])
    func oneCredentialWaiterCanFinishWithoutPoisoningTheSharedOwner(cancelled: Bool) async throws {
        let clock = MutationTestClock()
        let credentials = SharedDeadlineCredentials()
        let peer = DeadlineRelayPeer(clock: clock)
        let session = try await peer.session(bearer: { try await credentials.token() })
        let firstRunner = MutationRunner(clock: clock.clock)
        let nextRunner = MutationRunner(clock: clock.clock)
        var starts = await credentials.states.makeAsyncIterator()
        let first = Task {
            try await firstRunner.run(unknown: { RelayOperationResult.unknown }) {
                try await session.execute(requestID: "expired-auth", method: "POST", path: "/actions",
                    body: Data(#"{"command_id":"semantic-command"}"#.utf8))
            }
        }
        #expect(await starts.next() == 1)
        clock.advance(by: .seconds(60))
        let next = Task {
            try await nextRunner.run(unknown: { RelayOperationResult.unknown }) {
                try await session.execute(requestID: "surviving-auth", method: "POST", path: "/actions",
                    body: Data(#"{"command_id":"semantic-command"}"#.utf8))
            }
        }
        #expect(await starts.next() == 2)
        if cancelled { first.cancel() } else { clock.advance(by: .seconds(36)) }
        let early = await first.result
        if case let .failure(error) = early {
            #expect(cancelled ? error is CancellationError : (error as? URLError)?.code == .timedOut)
        } else { Issue.record("the first waiter must finish before any mutation dispatch") }
        #expect(await firstRunner.runningWorkers.count == 1)
        #expect(await !credentials.inheritedBudget, "shared refresh belongs to its owner, independent of either caller")
        await peer.complete()
        await credentials.releaseAndDrain()
        let surviving = await next.result
        #expect((try? surviving.get()) == .completed(status: 200, body: Data(#"{"ok":true}"#.utf8)))
        await firstRunner.drain()
        await nextRunner.drain()
        #expect(await peer.postIDs == ["surviving-auth"])
        #expect(await peer.effects == 1)
        #expect(await firstRunner.runningWorkers.isEmpty)
        #expect(await nextRunner.runningWorkers.isEmpty)
        #expect(await firstRunner.timerTask == nil)
        #expect(await nextRunner.timerTask == nil)
        await session.deactivate()
        await session.mutations.drain()
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

    @Test func aLateMutationAnswerStaysUnknownAndTheSameSessionCanRecoverWithoutAnotherPOST() async throws {
        let clock = MutationTestClock()
        let peer = DeadlineRelayPeer(clock: clock, postDuration: .seconds(120), completed: true)
        let session = try await peer.session()
        let body = Data(#"{"command_id":"semantic-command"}"#.utf8)
        let result = try await session.execute(requestID: "late-mutation", method: "POST", path: "/actions", body: body)
        #expect(result == .unknown, "the peer ignores cancellation and its advertised HTTP timeout")
        #expect(try await session.recoveryStatus(requestID: "late-mutation") == .completed(status: 200, body: Data(#"{"ok":true}"#.utf8)))
        let gate = HeldMutationWork()
        var starts = await gate.startStates.makeAsyncIterator()
        let cancelled = Task {
            await gate.perform()
            return try await session.execute(requestID: "late-mutation", method: "POST", path: "/actions", body: body)
        }
        #expect(await starts.next() == 1)
        cancelled.cancel()
        await gate.release()
        #expect((try? await cancelled.value) == .unknown)
        #expect(await peer.postIDs == ["late-mutation"])
        #expect(await peer.effects == 1)
        try await session.retireAppliedResponse(requestID: "late-mutation")
        #expect(await session.sentEnvelope(for: "late-mutation") == nil)
        await session.deactivate()
        await session.mutations.drain()
    }
}

extension MutationDeadlineTests {
    @Test(arguments: [false, true])
    func aCancelledDuplicateStaysUnknownWhileTheOriginalCanStillSendOrHasApplied(heldPOST: Bool) async throws {
        let clock = MutationTestClock()
        let credentials = SharedDeadlineCredentials()
        let post = HeldMutationWork()
        let peer = DeadlineRelayPeer(clock: clock, postGate: heldPOST ? post : nil)
        let session = try await peer.session(bearer: { try await credentials.token() })
        let body = Data(#"{"command_id":"semantic-command"}"#.utf8)
        var credentialStarts = await credentials.states.makeAsyncIterator()
        var postStarts = await post.startStates.makeAsyncIterator()
        let original = Task {
            try await session.execute(requestID: "shared-operation", method: "POST", path: "/actions", body: body, statusPolls: 0)
        }
        #expect(await credentialStarts.next() == 1)
        if heldPOST {
            await credentials.releaseAndDrain()
            #expect(await postStarts.next() == 1)
        }
        let gate = HeldMutationWork()
        var starts = await gate.startStates.makeAsyncIterator()
        let duplicate = Task {
            await gate.perform()
            return try await session.execute(requestID: "shared-operation", method: "POST", path: "/actions", body: body)
        }
        #expect(await starts.next() == 1)
        duplicate.cancel()
        await gate.release()
        #expect((try? await duplicate.value) == .unknown, "another owner may still send this operation ID")
        #expect(await peer.posts == (heldPOST ? 1 : 0))
        if heldPOST { await post.release() } else { await credentials.releaseAndDrain() }
        #expect(try await original.value == .unknown)
        await peer.complete()
        #expect(try await session.recoveryStatus(requestID: "shared-operation") == .completed(status: 200, body: Data(#"{"ok":true}"#.utf8)))
        #expect(await peer.statusTimeouts == [15], "the same session resolves uncertainty by GET")
        #expect(await peer.posts == 1)
        #expect(await peer.effects == 1)
        try await session.retireAppliedResponse(requestID: "shared-operation")
        #expect(await session.sentEnvelope(for: "shared-operation") == nil)
        await session.deactivate()
        await session.mutations.drain()
    }
}

extension MutationDeadlineTests {
    @Test func dispatchEvidenceIsSharedWhileTheFirstOwnerCanStillSend() async throws {
        let clock = MutationTestClock()
        let evidence = MutationDispatchEvidence()
        let credentials = SharedDeadlineCredentials()
        let peer = DeadlineRelayPeer(clock: clock)
        let session = try await peer.session(bearer: { try await credentials.token() })
        let firstRunner = MutationRunner(clock: clock.clock)
        let duplicateRunner = MutationRunner(clock: clock.clock)
        var starts = await credentials.states.makeAsyncIterator()
        let body = Data(#"{"command_id":"semantic-command"}"#.utf8)
        let original = Task {
            try await firstRunner.run(dispatchEvidence: evidence, unknown: { RelayOperationResult.unknown }) {
                try MutationScope.reserve(evidence)
                return try await session.execute(requestID: "shared-evidence", method: "POST", path: "/actions", body: body, statusPolls: 0)
            }
        }
        #expect(await starts.next() == 1)
        let gate = HeldMutationWork()
        var gates = await gate.startStates.makeAsyncIterator()
        let duplicate = Task {
            await gate.perform()
            return try await duplicateRunner.run(dispatchEvidence: evidence, unknown: { RelayOperationResult.unknown }) {
                Issue.record("a cancelled duplicate cannot begin reconciliation or submission")
                return try await session.execute(requestID: "shared-evidence", method: "POST", path: "/actions", body: body)
            }
        }
        #expect(await gates.next() == 1)
        duplicate.cancel()
        await gate.release()
        #expect((try? await duplicate.value) == .unknown)
        #expect(await peer.posts == 0, "the first reservation is uncertainty, not evidence of a POST")
        await credentials.releaseAndDrain()
        #expect(try await original.value == .unknown)
        await firstRunner.drain()
        await duplicateRunner.drain()
        await peer.complete()
        #expect(try await session.recoveryStatus(requestID: "shared-evidence") == .completed(status: 200, body: Data(#"{"ok":true}"#.utf8)))
        #expect(await peer.posts == 1)
        #expect(await peer.effects == 1)
        await session.deactivate()
        await session.mutations.drain()
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
    func aLateRelayReceiptIsKeptAfterItsSubmissionOrRecoveryCallerExpires(recovery: Bool) async throws {
        let clock = MutationTestClock()
        let held = HeldMutationWork()
        let peer = DeadlineRelayPeer(clock: clock, postGate: recovery ? nil : held, statusGate: recovery ? held : nil)
        let session = try await peer.session()
        let body = Data(#"{"command_id":"semantic-command"}"#.utf8)
        let runner = MutationRunner(clock: clock.clock)
        var starts = await held.startStates.makeAsyncIterator()
        if recovery {
            #expect(try await session.execute(requestID: "late-receipt", method: "POST", path: "/actions", body: body, statusPolls: 0) == .unknown)
            await peer.complete()
        }
        let call = Task {
            try await runner.run(knownDispatched: !recovery, conservativeUnknown: recovery, unknown: { RelayOperationResult.unknown }) {
                if recovery { return try await session.resume(requestID: "late-receipt") }
                return try await session.execute(requestID: "late-receipt", method: "POST", path: "/actions", body: body, statusPolls: 0)
            }
        }
        #expect(await starts.next() == 1)
        #expect(await peer.effects == 1)
        clock.advance(by: .seconds(120))
        #expect(try await call.value == .unknown)
        #expect(await runner.runningWorkers.count == 1)
        await peer.complete()
        await held.release()
        await runner.drain()
        #expect(try await session.recoveryStatus(requestID: "late-receipt") == .completed(status: 200, body: Data(#"{"ok":true}"#.utf8)))
        #expect(await peer.posts == 1)
        #expect(await peer.effects == 1)
        #expect(await peer.statusTimeouts == (recovery ? [15] : []))
        try await session.retireAppliedResponse(requestID: "late-receipt")
        #expect(await session.sentEnvelope(for: "late-receipt") == nil)
        await session.deactivate()
        await session.mutations.drain()
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
        let clock = MutationTestClock()
        let peer = DeadlineRelayPeer(clock: clock)
        let session = try await peer.session()
        let body = oversized
            ? try JSONSerialization.data(withJSONObject: ["command_id": "semantic-command", "padding": String(repeating: "x", count: 300_000)])
            : Data("{}".utf8)
        if oversized {
            await #expect(throws: RelayOutboxError.invalidEnvelope) {
                try await session.execute(requestID: "invalid-envelope", method: "POST", path: "/actions", body: body)
            }
        } else {
            await #expect(throws: RelaySessionError.invalidRequest) {
                try await session.execute(requestID: "invalid-envelope", method: "POST", path: "/actions#invalid", body: body)
            }
        }
        await session.mutations.drain()
        #expect(await session.mutationEvidence.entryCount == 0)
        #expect(try await session.recoveryStatus(requestID: "invalid-envelope") == .notDispatched)
        #expect(await peer.posts == 0)
        #expect(await peer.effects == 0)
        await session.deactivate()
        await session.mutations.drain()
    }

    @Test(arguments: [false, true])
    func standaloneRelayStatusUsesANewGETOnlyBudget(recovery: Bool) async throws {
        let clock = MutationTestClock()
        let peer = DeadlineRelayPeer(clock: clock)
        let session = try await peer.session()
        #expect(try await session.execute(requestID: "standalone-status", method: "POST", path: "/actions",
            body: Data(#"{"command_id":"semantic-command"}"#.utf8), statusPolls: 0) == .unknown)
        await session.mutations.drain()
        let startedAt = clock.clock.now()
        if recovery { #expect(try await session.recoveryStatus(requestID: "standalone-status") == .unknown) }
        else { #expect(try await session.resume(requestID: "standalone-status") == .unknown) }
        await session.mutations.drain()
        #expect(startedAt.duration(to: clock.clock.now()) == .seconds(120))
        #expect(await peer.statusTimeouts == [15, 15, 15, 15, 15, 15, 15, 15])
        #expect(await peer.posts == 1)
        await peer.complete()
        #expect(try await session.recoveryStatus(requestID: "standalone-status") == .completed(status: 200, body: Data(#"{"ok":true}"#.utf8)))
        #expect(await peer.posts == 1)
        #expect(await peer.effects == 1)
        await session.deactivate()
        await session.mutations.drain()
    }
}

extension MutationDeadlineTests {
    @Test func cancellingStandaloneRecoveryFinishesTheCallerAndKeepsTheHeldGETOwned() async throws {
        let clock = MutationTestClock()
        let held = HeldMutationWork()
        let peer = DeadlineRelayPeer(clock: clock, statusGate: held)
        let session = try await peer.session()
        #expect(try await session.execute(requestID: "cancelled-recovery", method: "POST", path: "/actions",
            body: Data(#"{"command_id":"semantic-command"}"#.utf8), statusPolls: 0) == .unknown)
        await session.mutations.drain()
        await peer.complete()
        var starts = await held.startStates.makeAsyncIterator()
        let call = Task { try await session.resume(requestID: "cancelled-recovery") }
        #expect(await starts.next() == 1)
        call.cancel()
        #expect(try await call.value == .unknown)
        #expect(await session.mutations.runningWorkers.count == 1)
        await held.release()
        await session.mutations.drain()
        #expect(try await session.recoveryStatus(requestID: "cancelled-recovery") == .completed(status: 200, body: Data(#"{"ok":true}"#.utf8)))
        #expect(await peer.statusTimeouts == [15])
        #expect(await peer.posts == 1)
        #expect(await peer.effects == 1)
        await session.deactivate()
        await session.mutations.drain()
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
        var peers: [DeadlineRelayPeer] = []
        var sessions: [RelaySession] = []
        for _ in 0..<5 {
            let peer = DeadlineRelayPeer(clock: clock)
            peers.append(peer)
            sessions.append(try await peer.session())
        }
        let prepared = sessions
        var rejected = false
        do {
            _ = try await root.run(unknown: { APIResponse.unknown }) {
                for (index, session) in prepared.enumerated() {
                    _ = try await session.execute(requestID: "tracked-limit-\(index)", method: "POST", path: "/actions",
                        body: Data(#"{"command_id":"semantic-command"}"#.utf8), statusPolls: 0)
                }
                return APIResponse.completed(status: 200, body: Data())
            }
        } catch RelaySessionError.invalidRequest { rejected = true }
        await root.drain()
        for session in sessions { await session.mutations.drain() }
        #expect(rejected)
        for index in 0..<4 {
            #expect(await peers[index].posts == 1)
            #expect(await peers[index].effects == 1)
            #expect(await sessions[index].mutationEvidence.entryCount == 1)
        }
        #expect(await peers[4].posts == 0)
        #expect(await peers[4].effects == 0)
        #expect(await sessions[4].sentEnvelope(for: "tracked-limit-4") == nil)
        #expect(await sessions[4].mutationEvidence.entryCount == 0)
        for session in sessions {
            await session.deactivate()
            await session.mutations.drain()
        }
    }
}
