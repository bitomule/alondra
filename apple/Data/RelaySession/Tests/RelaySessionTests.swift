import AlondraAPIClient
import AlondraRelayClient
import AlondraRelayOutbox
import AlondraRelaySession
import CryptoKit
import Foundation
import XCTest

final class RelaySessionTests: XCTestCase {
    func testRecoveryDistinguishesProvenAbsenceFromExecutedOrUncertainMutations() async throws {
        for mode in [FakeHTTP.Mode.success, .oversized, .rejected] {
            let fixture = try Fixture(mode: mode)
            defer { try? FileManager.default.removeItem(at: fixture.directory) }
            let absent = try await fixture.session().recoveryStatus(requestID: "operation-1")
            XCTAssertEqual(absent, .notDispatched)
            _ = try await fixture.session().execute(requestID: "operation-1", method: "POST", path: "/v1/nodes/node/message")
            let restored = try await fixture.session().recoveryStatus(requestID: "operation-1")
            switch mode {
            case .success: XCTAssertEqual(restored, .completed(status: 200, body: Data(#"{"ok":true}"#.utf8)))
            case .rejected: XCTAssertEqual(restored, .completed(status: 422, body: Data(#"{"error":"invalid message"}"#.utf8)))
            default: XCTAssertEqual(restored, .unknown)
            }
            let posts = await fixture.http.mutations
            XCTAssertEqual(posts.count, 1, "Recovery may only query an existing envelope")
        }
    }

    func testReadCannotReuseMutationIdentityInEitherDirection() async throws {
        let fixture = try Fixture()
        defer { try? FileManager.default.removeItem(at: fixture.directory) }
        let session = fixture.session()
        _ = try await session.execute(requestID: "operation-1", method: "POST", path: "/v1/nodes/node/message")
        _ = try await session.execute(requestID: "operation-1", method: "GET", path: "/v1/overview")
        let requests = await fixture.http.mutations
        XCTAssertEqual(requests.count, 2)
        let read = try JSONDecoder().decode(EnvelopeV1.self, from: requests[1])
        XCTAssertNotEqual(read.requestID, "operation-1")
        XCTAssertTrue(read.requestID.hasPrefix("read-"))
        do {
            _ = try await session.execute(requestID: read.requestID, method: "POST", path: "/v1/nodes/node/message")
            XCTFail("Mutation must not reuse an internal read identity")
        } catch RelaySessionError.invalidRequest { }
        let after = await fixture.http.mutations
        XCTAssertEqual(after.count, 2)
    }

    func testMutationIdentityRejectsChangedRequestAfterRelaunchAndRetirement() async throws {
        let fixture = try Fixture()
        defer { try? FileManager.default.removeItem(at: fixture.directory) }
        let session = fixture.session()
        _ = try await session.execute(requestID: "operation-1", method: "POST", path: "/v1/nodes/node/message",
                                      body: Data(#"{"text":"hello"}"#.utf8))
        for retired in [false, true] {
            if retired { try await session.retireAppliedResponse(requestID: "operation-1") }
            let restored = fixture.session()
            for (method, path, body) in [
                ("POST", "/v1/nodes/node/message", #"{"text":"changed"}"#),
                ("POST", "/v1/nodes/other/message", #"{"text":"hello"}"#),
                ("DELETE", "/v1/nodes/node/message", #"{"text":"hello"}"#)
            ] {
                do {
                    _ = try await restored.execute(requestID: "operation-1", method: method, path: path, body: Data(body.utf8))
                    XCTFail("Changed operation must not receive the original operation's success")
                } catch RelaySessionError.operationIdentityConflict { }
            }
        }
        let posts = await fixture.http.mutations
        XCTAssertEqual(posts.count, 1)
    }

    func testConcurrentDifferentMutationsCannotShareAnIdentity() async throws {
        let fixture = try Fixture()
        defer { try? FileManager.default.removeItem(at: fixture.directory) }
        @Sendable func attempt(_ text: String) async throws -> Bool {
            do {
                let body = try JSONSerialization.data(withJSONObject: ["text": text])
                let outcome = try await fixture.session().execute(requestID: "operation-1", method: "POST",
                    path: "/v1/nodes/node/message", body: body)
                XCTAssertEqual(outcome, .completed(status: 200, body: Data(#"{"ok":true}"#.utf8)))
                return true
            } catch RelaySessionError.operationIdentityConflict { return false }
        }
        async let first = attempt("first")
        async let second = attempt("second")
        let results = try await [first, second]
        XCTAssertEqual(results.filter { $0 }.count, 1)
        let posts = await fixture.http.mutations
        XCTAssertEqual(posts.count, 1)
    }

    func testReadRecoversLostResponseWithoutCreatingDurableMutationJournal() async throws {
        let fixture = try Fixture()
        defer { try? FileManager.default.removeItem(at: fixture.directory) }
        let api = AlondraAPIClient(serviceURL: fixture.scope.relay, deviceID: fixture.scope.deviceID,
                                   operations: fixture.session())
        let response = try await api.send(Endpoint<Data>.data(.get, path: ["v1", "overview"]),
                                          operationID: "operation-1")
        XCTAssertEqual(response, Data(#"{"ok":true}"#.utf8))
        let posts = await fixture.http.mutations
        XCTAssertEqual(posts.count, 1, "Lost read replies resolve through status, not another POST")
        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.directory.appendingPathComponent("outbox.json").path),
                       "Periodic reads must not accumulate permanent mutation tombstones")
    }

    func testUnifiedAPIRecoversLostMutationAcrossCoordinatorRecreation() async throws {
        let fixture = try Fixture()
        defer { try? FileManager.default.removeItem(at: fixture.directory) }
        let endpoint = Endpoint<Data>.data(.post, path: ["v1", "nodes", "node", "message"],
                                           body: Data(#"{"text":"hello"}"#.utf8))
        let first = AlondraAPIClient(serviceURL: fixture.scope.relay, deviceID: fixture.scope.deviceID,
                                    operations: fixture.session())
        let result = try await first.send(endpoint, operationID: "operation-1")
        XCTAssertEqual(result, Data(#"{"ok":true}"#.utf8))
        let restored = AlondraAPIClient(serviceURL: fixture.scope.relay, deviceID: fixture.scope.deviceID,
                                       operations: fixture.session())
        let recovered = try await restored.send(endpoint, operationID: "operation-1")
        XCTAssertEqual(recovered, result)
        let posts = await fixture.http.mutations
        XCTAssertEqual(posts.count, 1, "The product API must preserve the durable ID across recreation")
    }

    func testLostPOSTResponseRelaunchResolvesEncryptedResultWithoutReplayingMutation() async throws {
        let fixture = try Fixture()
        defer { try? FileManager.default.removeItem(at: fixture.directory) }
        let first = fixture.session()
        let initial = try await first.execute(requestID: "operation-1", method: "POST", path: "/v1/nodes/node/message",
                                              body: Data(#"{"text":"hello"}"#.utf8), statusPolls: 0)
        XCTAssertEqual(initial, .unknown)
        let restored = fixture.session()
        let result = try await restored.execute(requestID: "operation-1", method: "POST", path: "/v1/nodes/node/message",
                                                body: Data(#"{"text":"hello"}"#.utf8))
        XCTAssertEqual(result, .completed(status: 200, body: Data(#"{"ok":true}"#.utf8)))
        let requests = await fixture.http.mutations
        XCTAssertEqual(requests.count, 1, "A restored operation must never duplicate the Mac message")
        let envelope = try JSONDecoder().decode(EnvelopeV1.self, from: XCTUnwrap(requests.first))
        let plaintext = try envelope.open(senderKey: fixture.phoneSigning.publicKey, recipientKey: fixture.macAgreement,
            sender: "phone-test", recipient: "mac-test", route: "mac-test", nowMilliseconds: 1000)
        let local = try XCTUnwrap(JSONSerialization.jsonObject(with: plaintext) as? [String: Any])
        XCTAssertEqual(local["path"] as? String, "/v1/nodes/node/message")
        XCTAssertEqual((local["body"] as? [String: String])?["text"], "hello")
    }

    func testRestartRefreshesChallengeForStatusWithoutRepostingMutation() async throws {
        let fixture = try Fixture(mode: .restart)
        defer { try? FileManager.default.removeItem(at: fixture.directory) }
        let result = try await fixture.session().execute(requestID: "operation-1", method: "POST", path: "/v1/nodes/node/message")
        XCTAssertEqual(result, .completed(status: 200, body: Data(#"{"ok":true}"#.utf8)))
        let mutations = await fixture.http.mutations
        let headers = await fixture.http.statusEpochs
        XCTAssertEqual(mutations.count, 1)
        XCTAssertEqual(headers, ["AAAAAAAAAAAAAAAAAAAAAA", "AQEBAQEBAQEBAQEBAQEBAQ"])
    }

    func testContentLongPollUsesEncryptedDaemonCursorAndNoMutationJournal() async throws {
        let fixture = try Fixture(mode: .overview)
        defer { try? FileManager.default.removeItem(at: fixture.directory) }
        let session = fixture.session()
        try await session.waitForChange(afterSnapshotTick: UInt64.max, seconds: 50)
        let posts = await fixture.http.mutations
        XCTAssertEqual(posts.count, 1)
        let envelope = try JSONDecoder().decode(EnvelopeV1.self, from: XCTUnwrap(posts.first))
        let plaintext = try envelope.open(senderKey: fixture.phoneSigning.publicKey, recipientKey: fixture.macAgreement,
            sender: "phone-test", recipient: "mac-test", route: "mac-test", nowMilliseconds: 1000)
        let request = try XCTUnwrap(JSONSerialization.jsonObject(with: plaintext) as? [String: Any])
        XCTAssertEqual(request["method"] as? String, "GET")
        XCTAssertEqual(request["path"] as? String, "/v1/changes?after=18446744073709551615&wait=50")
        XCTAssertNil(request["body"])
        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.directory.appendingPathComponent("outbox.json").path))
    }

    func testContentLongPollWaitsForRemoteResultAndLogoutSuppressesNextFetch() async throws {
        let fixture = try Fixture(mode: .pendingChange)
        defer { try? FileManager.default.removeItem(at: fixture.directory) }
        let timer = TimerGate()
        let session = fixture.session(sleep: { try await timer.wait(.seconds(1)) })
        _ = try await session.fetchOverview()
        let nextRefresh = Task {
            try await session.waitForChange(afterSnapshotTick: 999, seconds: 10)
            return try await session.fetchOverview()
        }
        let duration = await timer.started()
        XCTAssertEqual(duration, .seconds(1))
        let beforeRelease = await fixture.http.mutations
        XCTAssertEqual(beforeRelease.count, 2, "One overview and one content poll; no next overview before its response")
        await session.deactivate()
        await timer.release()
        do { _ = try await nextRefresh.value; XCTFail("Logged-out session fetched again") }
        catch is CancellationError { }
        let afterRelease = await fixture.http.mutations
        XCTAssertEqual(afterRelease.count, 2)
    }

    func testUncertainMacExecutionAndForgedIdentityCannotBecomeSuccess() async throws {
        for mode in [FakeHTTP.Mode.oversized, .wrongSender, .wrongRequest, .expired] {
            let fixture = try Fixture(mode: mode)
            defer { try? FileManager.default.removeItem(at: fixture.directory) }
            let session = fixture.session()
            let result = try await session.execute(requestID: "operation-1", method: "POST", path: "/v1/nodes/node/message")
            XCTAssertEqual(result, .unknown)
            let mutations = await fixture.http.mutations
            XCTAssertEqual(mutations.count, 1)
        }
    }
}

private struct Fixture {
    let phoneSigning = P256.Signing.PrivateKey()
    let phoneAgreement = P256.KeyAgreement.PrivateKey()
    let macSigning = P256.Signing.PrivateKey()
    let macAgreement = P256.KeyAgreement.PrivateKey()
    let directory: URL
    let http: FakeHTTP
    let scope = RelayOutboxScope(relay: URL(string: "https://relay.example")!, macID: "mac-test",
                                 deviceID: "phone-test", credentialRevision: "approved-generation-1")
    init(mode: FakeHTTP.Mode = .success) throws {
        directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        http = FakeHTTP(macSigning: macSigning, phoneAgreement: phoneAgreement.publicKey, mode: mode)
    }
    func session(sleep: @escaping @Sendable () async throws -> Void = {}) -> RelaySession {
        return try! RelaySession(journalFile: directory.appendingPathComponent("outbox.json"), scope: scope,
            bearer: { "token" }, transport: http,
            signing: phoneSigning, agreement: phoneAgreement, macSigning: macSigning.publicKey,
            macAgreement: macAgreement.publicKey, now: { 1000 }, sleep: sleep)
    }
}

private actor FakeHTTP: RelayHTTPTransport {
    enum Mode { case success, oversized, rejected, wrongSender, wrongRequest, expired, restart, overview, pendingChange }
    let macSigning: P256.Signing.PrivateKey
    let phoneAgreement: P256.KeyAgreement.PublicKey
    let mode: Mode
    var mutations: [Data] = []
    var statusEpochs: [String] = []
    var restarted = false
    init(macSigning: P256.Signing.PrivateKey, phoneAgreement: P256.KeyAgreement.PublicKey, mode: Mode) {
        self.macSigning = macSigning; self.phoneAgreement = phoneAgreement; self.mode = mode
    }
    func send(_ request: URLRequest) async throws -> RelayHTTPResponse {
        if request.url?.path == "/v1/bootstrap" {
            let epoch = restarted ? "AQEBAQEBAQEBAQEBAQEBAQ" : "AAAAAAAAAAAAAAAAAAAAAA"
            return RelayHTTPResponse(status: 200, body: try JSONSerialization.data(withJSONObject: ["protocol": 1, "epoch": epoch]))
        }
        if request.httpMethod == "POST" {
            mutations.append(try XCTUnwrap(request.httpBody))
            throw URLError(.timedOut)
        }
        statusEpochs.append(request.value(forHTTPHeaderField: "x-relay-epoch") ?? "")
        if mode == .restart && !restarted {
            restarted = true
            return RelayHTTPResponse(status: 401, body: Data())
        }
        let requestID = try JSONDecoder().decode(EnvelopeV1.self, from: XCTUnwrap(mutations.last)).requestID
        if mode == .pendingChange && mutations.count > 1 {
            return RelayHTTPResponse(status: 200, body: try JSONSerialization.data(withJSONObject: ["request_id": requestID, "status": "pending", "revision": 1]))
        }
        let payload = mode == .overview || mode == .pendingChange ? #"{"status":200,"body":{"tick":42}}"# : mode == .oversized ? #"{"status":502,"error":"response_too_large"}"# : mode == .rejected ? #"{"status":422,"body":{"error":"invalid message"}}"# : #"{"status":200,"body":{"ok":true}}"#
        let envelope = try EnvelopeV1.seal(route: "mac-test", sender: mode == .wrongSender ? "mac-other" : "mac-test",
            recipient: "phone-test", requestID: mode == .wrongRequest ? "operation-other" : requestID, kind: "response",
            expiresAtMilliseconds: mode == .expired ? 999 : 5000, plaintext: Data(payload.utf8),
            senderKey: macSigning, recipientKey: phoneAgreement)
        var json = [String: Any]()
        json["request_id"] = requestID; json["status"] = "completed"; json["revision"] = 4
        json["response"] = try JSONSerialization.jsonObject(with: JSONEncoder().encode(envelope))
        return RelayHTTPResponse(status: 200, body: try JSONSerialization.data(withJSONObject: json))
    }
}

private actor TimerGate {
    private var waiting: CheckedContinuation<Void, Error>?
    private var observer: CheckedContinuation<Duration, Never>?
    private var duration: Duration?
    func wait(_ value: Duration) async throws {
        duration = value
        try await withCheckedThrowingContinuation { continuation in
            waiting = continuation
            observer?.resume(returning: value)
            observer = nil
        }
    }
    func started() async -> Duration {
        if let duration { return duration }
        return await withCheckedContinuation { observer = $0 }
    }
    func release() { waiting?.resume(); waiting = nil }
}
