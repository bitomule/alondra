@testable import AlondraTransport
import CryptoKit
import Foundation
import Testing

private enum ReadFault: Sendable, Equatable {
    case changesNetwork, changesBusy, completedStatus, lostSubmission, deadline, cancelled
    case changedEpoch, wrongResponseIdentity, refusedStatus, none
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
    private(set) var responseBytes = 0
    private(set) var exchangeCount = 0

    init(_ fault: ReadFault, requestWait: Bool = false) {
        self.fault = fault; self.requestWait = requestWait
        let pair = AsyncStream<Void>.makeStream(bufferingPolicy: .bufferingNewest(1))
        actionEntered = pair.stream; actionContinuation = pair.continuation
    }

    func session() throws -> RelaySession {
        try RelaySession(scope: RelayOutboxScope(relay: URL(string: "https://relay.fixture.invalid")!,
            macID: "mac", deviceID: "phone", credentialRevision: "fixture"), bearer: { "fixture-token" },
            transport: self, signing: phoneSigning, agreement: phoneAgreement,
            macSigning: macSigning.publicKey, macAgreement: macAgreement.publicKey, now: { 1_000 },
            sleep: { await self.backoff() }, waitForReadDeadline: time.clock.waitUntil, requestClock: time.clock)
    }

    func backoff() {
        backoffs += 1
        time.advance(by: fault == .deadline ? .seconds(16) : .seconds(1))
    }

    func releaseAction() {
        actionWaiter?.resume(); actionWaiter = nil
    }

    func send(_ request: URLRequest) async throws -> HTTPReply {
        exchangeCount += 1
        let url = try #require(request.url)
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
            switch fault {
            case .changesNetwork, .deadline: throw URLError(.networkConnectionLost)
            case .changesBusy: return HTTPReply(status: 429, body: Data())
            case .cancelled: throw CancellationError()
            default: break
            }
            let id = try #require(readID)
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
        }
        statusIDs.append(id)
        if id == "action-A" {
            actionContinuation.yield(())
            await withCheckedContinuation { actionWaiter = $0 }
        } else {
            #expect(id == readID)
            readStatuses += 1
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
        #expect(try await session.execute(requestID: "detail-B", method: "GET",
            path: "/views/card:B?protocol=1&client=ios&lang=en") == .unknown)
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
}
