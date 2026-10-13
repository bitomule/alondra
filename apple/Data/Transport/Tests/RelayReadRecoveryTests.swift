@testable import AlondraTransport
import CryptoKit
import Foundation
import Testing

private enum ReadFault: Sendable, Equatable {
    case changesNetwork, changesBusy, completedStatus, lostSubmission, deadline, cancelled
    case changedEpoch, wrongResponseIdentity, refusedStatus, none
    /// Degraded networks: every exchange slow, 30% of them lost, the relay unreachable for 20 s
    /// or for good, a read the relay never received, a job the Mac only gets after the relay's
    /// 30 s lease, and a Mac that never answers.
    case slowNetwork, lossy, relayCut, relayGone, neverQueued, leaseRedelivery, macSilent
}

/// SplitMix64: the same losses on every run.
private struct SeededLoss {
    private var state: UInt64
    init(seed: UInt64) { state = seed }
    mutating func drops(percent: UInt64) -> Bool {
        state &+= 0x9E37_79B9_7F4A_7C15
        var z = state
        z = (z ^ (z >> 30)) &* 0xBF58_476D_1CE4_E5B9
        z = (z ^ (z >> 27)) &* 0x94D0_49BB_1331_11EB
        return (z ^ (z >> 31)) % 100 < percent
    }
}

private actor ReadRecoveryPeer: HTTPTransport {
    let phoneSigning = try! P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 1, count: 32))
    let phoneAgreement = try! P256.KeyAgreement.PrivateKey(rawRepresentation: Data(repeating: 2, count: 32))
    let macSigning = try! P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 3, count: 32))
    let macAgreement = try! P256.KeyAgreement.PrivateKey(rawRepresentation: Data(repeating: 4, count: 32))
    let time = MutationTestClock()
    let actionEntered: AsyncStream<Void>
    private let actionContinuation: AsyncStream<Void>.Continuation
    private let fault: ReadFault
    private let requestWait: Bool
    private let epoch = Base64URL.encode(Data(repeating: 7, count: 16))
    private var actionWaiter: CheckedContinuation<Void, Never>?
    private var responses: [String: Envelope] = [:]
    private(set) var submissions: [String] = []
    private(set) var statusIDs: [String] = []
    private(set) var actionEffects = 0
    private(set) var changes = 0
    private(set) var readID: String?
    private(set) var readStatuses = 0
    private(set) var backoffs = 0
    private(set) var backoffAttempts: [Int] = []
    private(set) var losses = 0
    private var loss = SeededLoss(seed: 42)
    private var lostOnce = false
    private let startedAt: ContinuousClock.Instant
    private(set) var responseBytes = 0
    private(set) var exchangeCount = 0

    init(_ fault: ReadFault, requestWait: Bool = false) {
        self.fault = fault; self.requestWait = requestWait
        startedAt = time.clock.now()
        let pair = AsyncStream<Void>.makeStream(bufferingPolicy: .bufferingNewest(1))
        actionEntered = pair.stream; actionContinuation = pair.continuation
    }

    func session() throws -> RelaySession {
        try RelaySession(scope: RelayOutboxScope(relay: URL(string: "https://relay.fixture.invalid")!,
            macID: "mac", deviceID: "phone", credentialRevision: "fixture"), bearer: { "fixture-token" },
            transport: self, signing: phoneSigning, agreement: phoneAgreement,
            macSigning: macSigning.publicKey, macAgreement: macAgreement.publicKey, now: { 1_000 },
            sleep: { await self.backoff($0) }, waitForReadDeadline: time.clock.waitUntil, requestClock: time.clock)
    }

    func backoff(_ attempt: Int) {
        backoffs += 1
        backoffAttempts.append(attempt)
        time.advance(by: fault == .deadline ? .seconds(41) : RelaySession.retryBackoff.delay(attempt: attempt, random: 1))
    }

    private var elapsed: Duration { startedAt.duration(to: time.clock.now()) }

    /// What a degraded network does to one exchange before it reaches the relay.
    private func degrade(_ url: URL) throws {
        if fault == .slowNetwork { time.advance(by: .seconds(5)) }
        guard url.path != "/v1/bootstrap" else { return }
        if fault == .relayGone || (fault == .relayCut && elapsed < .seconds(20)) { throw URLError(.cannotConnectToHost) }
        if fault == .lossy, loss.drops(percent: 30) {
            losses += 1
            throw URLError(losses.isMultiple(of: 2) ? .timedOut : .networkConnectionLost)
        }
    }

    func releaseAction() {
        actionWaiter?.resume(); actionWaiter = nil
    }

    func send(_ request: URLRequest) async throws -> HTTPReply {
        exchangeCount += 1
        let url = try #require(request.url)
        try degrade(url)
        if url.path == "/v1/bootstrap" {
            return try json(requestWait ? ["protocol": 1, "epoch": epoch, "request_wait": true] : ["protocol": 1, "epoch": epoch])
        }
        try verify(request)
        if request.httpMethod == "POST" {
            #expect(url.path == "/v1/requests")
            let envelope = try JSONDecoder().decode(Envelope.self, from: #require(request.httpBody))
            let plaintext = try envelope.open(senderKey: phoneSigning.publicKey, recipientKey: macAgreement,
                sender: "phone", recipient: "mac", route: "mac", nowMilliseconds: 1_000)
            let payload = try #require(try JSONSerialization.jsonObject(with: plaintext) as? [String: Any])
            if fault == .neverQueued, !lostOnce {
                lostOnce = true
                throw URLError(.networkConnectionLost)
            }
            submissions.append(envelope.requestID)
            let method = try #require(payload["method"] as? String)
            let body: [String: Any]
            if method == "POST" {
                #expect(payload["path"] as? String == "/actions")
                actionEffects += 1
                body = ["card": "A", "applied": true]
            } else {
                #expect(method == "GET")
                #expect((payload["path"] as? String)?.hasPrefix("/views/card:B?") == true)
                #expect(envelope.requestID.hasPrefix("read-"))
                readID = envelope.requestID
                body = ["card": "B", "detail": "Second card"]
            }
            responses[envelope.requestID] = try Envelope.seal(route: "mac", sender: "mac", recipient: "phone",
                requestID: fault == .wrongResponseIdentity && method == "GET" ? "action-A" : envelope.requestID,
                kind: "response", expiresAtMilliseconds: 121_000,
                plaintext: JSONSerialization.data(withJSONObject: ["status": 200, "body": body], options: [.sortedKeys]),
                senderKey: macSigning, recipientKey: phoneAgreement.publicKey)
            if method == "GET", fault == .lostSubmission { throw URLError(.networkConnectionLost) }
            return try json(["request_id": envelope.requestID, "status": "queued", "revision": 1])
        }
        if url.path == "/v1/changes" {
            changes += 1
            if fault == .macSilent {
                time.advance(by: .seconds(25))
                return try json(["epoch": epoch, "revision": 1, "changes": []])
            }
            switch fault {
            case .changesNetwork, .deadline: throw URLError(.networkConnectionLost)
            case .changesBusy: return HTTPReply(status: 429, body: Data())
            case .cancelled: throw CancellationError()
            default: break
            }
            guard let id = readID else { return try json(["epoch": epoch, "revision": 1, "changes": []]) }
            return try json(["epoch": fault == .changedEpoch ? Base64URL.encode(Data(repeating: 8, count: 16)) : epoch,
                "revision": 2, "changes": [["request_id": id, "status": "completed", "revision": 2],
                    ["request_id": "action-A", "status": "completed", "revision": 2]]])
        }
        #expect(url.path.hasPrefix("/v1/requests/"))
        let id = url.lastPathComponent
        if url.query != nil {
            #expect(requestWait)
            #expect(url.query?.hasPrefix("wait_s=") == true)
            #expect(request.timeoutInterval == 35)
            if fault == .changesNetwork { throw URLError(.networkConnectionLost) }
            if fault == .cancelled { throw CancellationError() }
            if fault == .macSilent || (fault == .leaseRedelivery && elapsed < .seconds(31)) {
                let wait = Duration.seconds(try #require(url.query?.split(separator: "=").last.flatMap { Int($0) }))
                // The relay answers the long poll as soon as the re-leased job completes.
                time.advance(by: fault == .macSilent ? wait : min(wait, .seconds(31) - elapsed))
                if fault == .macSilent || elapsed < .seconds(31) {
                    return try json(["request_id": url.lastPathComponent, "status": "queued", "revision": 1])
                }
            }
        }
        statusIDs.append(id)
        if id == "action-A" {
            actionContinuation.yield(())
            await withCheckedContinuation { actionWaiter = $0 }
        } else {
            guard id == readID else { return HTTPReply(status: 404, body: Data()) }
            readStatuses += 1
            if fault == .macSilent { return try json(["request_id": id, "status": "queued", "revision": 1]) }
            if fault == .refusedStatus { return HTTPReply(status: 403, body: Data()) }
            if readStatuses == 1, fault == .completedStatus || fault == .lostSubmission {
                return HTTPReply(status: 503, body: Data())
            }
        }
        return try json(["request_id": id, "status": "completed", "revision": 2,
            "response": try JSONSerialization.jsonObject(with: JSONEncoder().encode(#require(responses[id])))])
    }

    private func verify(_ request: URLRequest) throws {
        let url = try #require(request.url)
        let components = try #require(URLComponents(url: url, resolvingAgainstBaseURL: false))
        let target = components.percentEncodedPath + (components.percentEncodedQuery.map { "?" + $0 } ?? "")
        let timestamp = try #require(request.value(forHTTPHeaderField: "x-device-timestamp-ms").flatMap(UInt64.init))
        let nonce = try Base64URL.decode(#require(request.value(forHTTPHeaderField: "x-device-nonce")))
        let signature = try P256.Signing.ECDSASignature(rawRepresentation:
            Base64URL.decode(#require(request.value(forHTTPHeaderField: "x-device-signature"))))
        #expect(request.value(forHTTPHeaderField: "x-device-id") == "phone")
        #expect(request.value(forHTTPHeaderField: "x-relay-epoch") == epoch)
        let message = try RelayHTTPChallenge.message(deviceID: "phone", method: #require(request.httpMethod),
            target: target, epoch: Base64URL.decode(epoch), body: request.httpBody ?? Data(),
            timestampMilliseconds: timestamp, nonce: nonce)
        #expect(phoneSigning.publicKey.isValidSignature(signature, for: message))
    }

    private func json(_ value: [String: Any]) throws -> HTTPReply {
        let body = try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
        responseBytes += body.count
        return HTTPReply(status: 200, body: body)
    }
}

@Suite struct RelayReadRecoveryTests {
    @Test func deferredReceiptRemovesOneExchangeWithoutChangingTheDetail() async throws {
        var exchanges: [Int] = []
        var responseBytes: [Int] = []
        for enabled in [false, true] {
            let peer = ReadRecoveryPeer(.none, requestWait: enabled)
            let session = try await peer.session()
            let result = try await session.execute(requestID: "detail-B", method: "GET",
                path: "/views/card:B?protocol=1&client=ios&lang=en")
            #expect(result == .completed(status: 200, body: Data(#"{"card":"B","detail":"Second card"}"#.utf8)))
            #expect(await peer.submissions.count == 1)
            #expect(await peer.readStatuses == 1)
            #expect(await peer.changes == (enabled ? 0 : 1))
            #expect(await peer.backoffs == 0)
            exchanges.append(await peer.exchangeCount)
            responseBytes.append(await peer.responseBytes)
            await session.deactivate()
        }
        #expect(exchanges == [4, 3])
        #expect(responseBytes[1] < responseBytes[0])
        print("deferred-read exchanges=\(exchanges) response_bytes=\(responseBytes) fixed_200ms_exchange_model_ms=\(exchanges.map { $0 * 200 }) correct_results=2 errors=0 posts=[1,1]")
    }

    @Test func deferredReceiptRecoversUsingTheSameReadIDWithoutResubmission() async throws {
        let peer = ReadRecoveryPeer(.changesNetwork, requestWait: true)
        let session = try await peer.session()
        #expect(try await session.execute(requestID: "detail-B", method: "GET",
            path: "/views/card:B?protocol=1&client=ios&lang=en") ==
            .completed(status: 200, body: Data(#"{"card":"B","detail":"Second card"}"#.utf8)))
        #expect(await peer.submissions.count == 1)
        #expect(await peer.changes == 0)
        #expect(await peer.backoffs == 1)
        let readID = try #require(await peer.readID)
        #expect(await peer.statusIDs == [readID])
        await session.deactivate()
    }

    @Test(arguments: [ReadFault.wrongResponseIdentity, .refusedStatus, .cancelled])
    private func deferredReceiptStillRejectsInvalidAndCancelledResults(_ fault: ReadFault) async throws {
        let peer = ReadRecoveryPeer(fault, requestWait: true)
        let session = try await peer.session()
        if fault == .cancelled {
            await #expect(throws: CancellationError.self) {
                try await session.execute(requestID: "detail-B", method: "GET",
                    path: "/views/card:B?protocol=1&client=ios&lang=en")
            }
        } else {
            #expect(try await session.execute(requestID: "detail-B", method: "GET",
                path: "/views/card:B?protocol=1&client=ios&lang=en") == .unknown)
        }
        #expect(await peer.submissions.count == 1)
        #expect(await peer.changes == 0)
        #expect(await peer.backoffs == 0)
        await session.deactivate()
    }

    @Test(arguments: [ReadFault.changesNetwork, .changesBusy, .completedStatus, .lostSubmission])
    private func detailRecoversWhileTheAppliedCardsReceiptIsStillPending(_ fault: ReadFault) async throws {
        let peer = ReadRecoveryPeer(fault)
        let session = try await peer.session()
        let action = Task { try await session.execute(requestID: "action-A", method: "POST", path: "/actions",
            body: Data(#"{"command_id":"action-A"}"#.utf8)) }
        var arrived = peer.actionEntered.makeAsyncIterator()
        _ = await arrived.next()
        let detail = try await session.execute(requestID: "detail-B", method: "GET",
            path: "/views/card:B?protocol=1&client=ios&lang=en")
        await peer.releaseAction()
        let applied = try await action.value
        #expect(detail == .completed(status: 200, body: Data(#"{"card":"B","detail":"Second card"}"#.utf8)))
        #expect(applied == .completed(status: 200, body: Data(#"{"applied":true,"card":"A"}"#.utf8)))
        #expect(await peer.actionEffects == 1)
        let ids = await peer.submissions
        #expect(ids.count == 2)
        #expect(ids.first == "action-A")
        #expect(ids.last == (await peer.readID))
        #expect(await peer.statusIDs.allSatisfy { $0 == "action-A" || $0 == ids.last })
        #expect(await peer.backoffs == 1)
        await session.deactivate()
        await session.mutations.drain()
    }

    @Test(arguments: [ReadFault.changedEpoch, .wrongResponseIdentity, .refusedStatus])
    private func terminalReadFailuresDoNotResubmitOrRetry(_ fault: ReadFault) async throws {
        let peer = ReadRecoveryPeer(fault)
        let session = try await peer.session()
        #expect(try await session.execute(requestID: "detail-B", method: "GET",
            path: "/views/card:B?protocol=1&client=ios&lang=en") == .unknown)
        #expect(await peer.submissions.count == 1)
        #expect(await peer.backoffs == 0)
        await session.deactivate()
    }

    @Test private func transientFailureCannotRenewTheReadDeadline() async throws {
        let peer = ReadRecoveryPeer(.deadline)
        let session = try await peer.session()
        await #expect(throws: URLError(.networkConnectionLost)) {
            try await session.execute(requestID: "detail-B", method: "GET",
                path: "/views/card:B?protocol=1&client=ios&lang=en")
        }
        #expect(await peer.submissions.count == 1)
        #expect(await peer.changes == 1)
        #expect(await peer.statusIDs.isEmpty)
        #expect(await peer.backoffs == 1)
        await session.deactivate()
    }

    @Test private func cancellingTheReadDoesNotStartStatusRecovery() async throws {
        let peer = ReadRecoveryPeer(.cancelled)
        let session = try await peer.session()
        await #expect(throws: CancellationError.self) {
            try await session.execute(requestID: "detail-B", method: "GET",
                path: "/views/card:B?protocol=1&client=ios&lang=en")
        }
        #expect(await peer.statusIDs.isEmpty)
        #expect(await peer.backoffs == 0)
        await session.deactivate()
    }

    @Test(arguments: [(ReadFault.slowNetwork, false), (.lossy, false), (.lossy, true), (.neverQueued, false),
                      (.relayCut, false), (.relayCut, true), (.leaseRedelivery, true)])
    private func degradedNetworksStillDeliverTheRead(_ fault: ReadFault, requestWait: Bool) async throws {
        let peer = ReadRecoveryPeer(fault, requestWait: requestWait)
        let session = try await peer.session()
        let result = try await session.execute(requestID: "detail-B", method: "GET",
            path: "/views/card:B?protocol=1&client=ios&lang=en")
        #expect(result == .completed(status: 200, body: Data(#"{"card":"B","detail":"Second card"}"#.utf8)))
        let submissions = await peer.submissions
        #expect(submissions.allSatisfy { $0 == submissions.first }, "a resent read is the same request")
        if fault == .neverQueued { #expect(await peer.backoffAttempts == [0]) }
        if fault == .relayCut {
            let attempts = await peer.backoffAttempts
            #expect(attempts == Array(attempts.indices), "each failure in a row waits longer: \(attempts)")
            #expect(attempts.count >= 5)
        }
        await session.deactivate()
    }

    @Test private func aRelayThatStaysUnreachableFailsWithTheNetworkError() async throws {
        let peer = ReadRecoveryPeer(.relayGone)
        let session = try await peer.session()
        await #expect(throws: URLError(.cannotConnectToHost)) {
            try await session.execute(requestID: "detail-B", method: "GET",
                path: "/views/card:B?protocol=1&client=ios&lang=en")
        }
        let attempts = await peer.backoffAttempts
        #expect(attempts.prefix(4) == [0, 1, 2, 3])
        await session.deactivate()
    }

    @Test(arguments: [false, true])
    private func aMacThatNeverAnswersEndsAsUnknownNotAsANetworkError(_ requestWait: Bool) async throws {
        let peer = ReadRecoveryPeer(.macSilent, requestWait: requestWait)
        let session = try await peer.session()
        #expect(try await session.execute(requestID: "detail-B", method: "GET",
            path: "/views/card:B?protocol=1&client=ios&lang=en") == .unknown)
        await session.deactivate()
    }
}
