import AlondraAPIClient
import AlondraContracts
import AlondraRelayOutbox
@testable import AlondraRelaySession
import CryptoKit
import Foundation
import XCTest

private let ok = Data(#"{"ok":true}"#.utf8)
private let changesBody = #"{"protocol_version":2,"data_epoch":"epoch","revision":42,"changed":true}"#
private let command = Data(#"{"protocol_version":2,"data_epoch":"epoch","command_id":"command-1","expected_revision":7,"command":{"kind":"mark_read","interaction_id":"interaction-1"}}"#.utf8)
private let changedCommand = Data(#"{"protocol_version":2,"data_epoch":"epoch","command_id":"command-1","expected_revision":8,"command":{"kind":"mark_read","interaction_id":"interaction-1"}}"#.utf8)

final class RelaySessionTests: XCTestCase {
    private func api(_ fixture: Fixture, _ session: RelaySession) -> AlondraAPIClient {
        AlondraAPIClient(serviceURL: fixture.scope.relay, deviceID: fixture.scope.deviceID, operations: session)
    }

    private func summary(_ fixture: Fixture, _ session: RelaySession) async throws -> Data {
        try await api(fixture, session).send(Endpoint<Data>.data(.get, path: ["v2", "domain", "summary"]))
    }

    private func changes(_ fixture: Fixture, _ session: RelaySession, after revision: UInt64) async throws -> DomainV2.Changes {
        try await api(fixture, session).domainChanges(epoch: "epoch", after: revision, wait: 50)
    }

    private func opened(_ fixture: Fixture, _ post: Data?) throws -> [String: Any] {
        let envelope = try JSONDecoder().decode(EnvelopeV1.self, from: XCTUnwrap(post))
        let plaintext = try envelope.open(senderKey: fixture.phoneSigning.publicKey, recipientKey: fixture.macAgreement,
            sender: "phone-test", recipient: "mac-test", route: "mac-test", nowMilliseconds: 1000)
        return try XCTUnwrap(JSONSerialization.jsonObject(with: plaintext) as? [String: Any])
    }

    func testInvalidEnvelopeTimeIsRejectedBeforeDispatchAndReservation() async throws {
        let fixture = try Fixture()
        let session = fixture.session(now: { UInt64.max })
        do {
            _ = try await session.execute(requestID: "operation-1", method: "POST", path: "/v2/domain/commands", body: command)
            XCTFail("Invalid envelope time must remain a definite rejection")
        } catch RelaySessionError.invalidRequest { }
        let posts = await fixture.http.mutations
        XCTAssertTrue(posts.isEmpty)
        let restored = try await session.recoveryStatus(requestID: "operation-1")
        XCTAssertEqual(restored, .notDispatched)
    }

    func testRecoveryDistinguishesProvenAbsenceFromExecutedOrUncertainMutations() async throws {
        for mode in [FakeHTTP.Mode.success, .oversized, .rejected] {
            let fixture = try Fixture(mode: mode)
            let session = fixture.session()
            let absent = try await session.recoveryStatus(requestID: "operation-1")
            XCTAssertEqual(absent, .notDispatched)
            _ = try await session.execute(requestID: "operation-1", method: "POST", path: "/v2/domain/commands", body: command)
            let restored = try await session.recoveryStatus(requestID: "operation-1")
            switch mode {
            case .success: XCTAssertEqual(restored, .completed(status: 200, body: ok))
            case .rejected: XCTAssertEqual(restored, .completed(status: 422, body: Data(#"{"error":"invalid message"}"#.utf8)))
            default: XCTAssertEqual(restored, .unknown)
            }
            let posts = await fixture.http.mutations
            XCTAssertEqual(posts.count, 1, "Recovery may only query an existing envelope")
        }
    }

    func testReadCannotReuseMutationIdentityInEitherDirection() async throws {
        let fixture = try Fixture()
        let session = fixture.session()
        _ = try await session.execute(requestID: "operation-1", method: "POST", path: "/v2/domain/commands", body: command)
        _ = try await session.execute(requestID: "operation-1", method: "GET", path: "/v2/domain/summary")
        let requests = await fixture.http.mutations
        XCTAssertEqual(requests.count, 2)
        let read = try JSONDecoder().decode(EnvelopeV1.self, from: requests[1])
        XCTAssertNotEqual(read.requestID, "operation-1")
        XCTAssertTrue(read.requestID.hasPrefix("read-"))
        do {
            _ = try await session.execute(requestID: read.requestID, method: "POST", path: "/v2/domain/commands", body: command)
            XCTFail("Mutation must not reuse an internal read identity")
        } catch RelaySessionError.invalidRequest { }
        let after = await fixture.http.mutations
        XCTAssertEqual(after.count, 2)
    }

    func testSectionedReadsAreAdmittedAndTheWholeProjectionIsRefusedBeforeAnyPOST() async throws {
        let fixture = try Fixture()
        let session = fixture.session()
        do {
            _ = try await session.execute(requestID: "whole", method: "GET", path: "/v2/domain")
            XCTFail("The whole projection is no longer read")
        } catch RelaySessionError.invalidRequest { }
        let none = await fixture.http.mutations
        XCTAssertTrue(none.isEmpty, "A refused read never reaches the relay")
        for path in ["/v2/domain/summary", "/v2/domain/inbox/interaction%3Aq"] {
            let result = try await session.execute(requestID: "caller-read", method: "GET", path: path)
            XCTAssertEqual(result, .completed(status: 200, body: ok))
        }
        let posts = await fixture.http.mutations
        XCTAssertEqual(try posts.map { try opened(fixture, $0)["path"] as? String },
                       ["/v2/domain/summary", "/v2/domain/inbox/interaction%3Aq"])
    }

    func testMutationIdentityRejectsChangedRequestBeforeAndAfterRetirement() async throws {
        let fixture = try Fixture()
        let session = fixture.session()
        _ = try await session.execute(requestID: "operation-1", method: "POST", path: "/v2/domain/commands", body: command)
        for retired in [false, true] {
            if retired { try await session.retireAppliedResponse(requestID: "operation-1") }
            for (method, path, body) in [
                ("POST", "/v2/domain/commands", changedCommand),
                ("POST", "/v2/domain/schedules", command),
                ("PUT", "/v2/domain/schedules/schedule-1", command)
            ] {
                do {
                    _ = try await session.execute(requestID: "operation-1", method: method, path: path, body: body)
                    XCTFail("Changed operation must not receive the original operation's success")
                } catch RelaySessionError.operationIdentityConflict { }
            }
        }
        let posts = await fixture.http.mutations
        XCTAssertEqual(posts.count, 1)
    }

    func testConcurrentDifferentMutationsCannotShareAnIdentity() async throws {
        let fixture = try Fixture()
        let session = fixture.session()
        @Sendable func attempt(_ body: Data) async throws -> Bool {
            do {
                let outcome = try await session.execute(requestID: "operation-1", method: "POST",
                    path: "/v2/domain/commands", body: body)
                XCTAssertEqual(outcome, .completed(status: 200, body: ok))
                return true
            } catch RelaySessionError.operationIdentityConflict { return false }
        }
        async let first = attempt(command)
        async let second = attempt(changedCommand)
        let results = try await [first, second]
        XCTAssertEqual(results.filter { $0 }.count, 1)
        let posts = await fixture.http.mutations
        XCTAssertEqual(posts.count, 1)
    }

    func testReadRecoversLostResponseWithoutReservingAnIdentity() async throws {
        let fixture = try Fixture(mode: .overview)
        let session = fixture.session()
        let response = try await api(fixture, session).send(Endpoint<Data>.data(.get, path: ["v2", "domain", "summary"]),
                                                            operationID: "operation-1")
        XCTAssertEqual(try DomainV2.decode(DomainV2.Changes.self, from: response),
                       try DomainV2.decode(DomainV2.Changes.self, from: Data(changesBody.utf8)))
        let posts = await fixture.http.mutations
        XCTAssertEqual(posts.count, 1, "Lost read replies resolve through status, not another POST")
        let recorded = try await session.recoveryStatus(requestID: "operation-1")
        XCTAssertEqual(recorded, .notDispatched, "Periodic reads must not accumulate mutation records")
    }

    func testUnifiedAPIRecoversLostMutationAcrossClientRecreation() async throws {
        let fixture = try Fixture()
        let session = fixture.session()
        let endpoint = Endpoint<Data>.data(.post, path: ["v2", "domain", "commands"], body: command)
        let result = try await api(fixture, session).send(endpoint, operationID: "command-1")
        XCTAssertEqual(result, ok)
        let recovered = try await api(fixture, session).send(endpoint, operationID: "command-1")
        XCTAssertEqual(recovered, result)
        let posts = await fixture.http.mutations
        XCTAssertEqual(posts.count, 1, "The product API keeps the operation ID across client values")
    }

    func testLostPOSTResponseResolvesEncryptedResultWithoutReplayingMutation() async throws {
        let fixture = try Fixture()
        let session = fixture.session()
        let initial = try await session.execute(requestID: "operation-1", method: "POST", path: "/v2/domain/commands",
                                                body: command, statusPolls: 0)
        XCTAssertEqual(initial, .unknown)
        let result = try await session.execute(requestID: "operation-1", method: "POST", path: "/v2/domain/commands",
                                               body: command)
        XCTAssertEqual(result, .completed(status: 200, body: ok))
        let requests = await fixture.http.mutations
        XCTAssertEqual(requests.count, 1, "A retried operation must never duplicate the command")
        let local = try opened(fixture, requests.first)
        XCTAssertEqual(local["path"] as? String, "/v2/domain/commands")
        XCTAssertEqual((local["body"] as? [String: Any])?["command_id"] as? String, "command-1")
    }

    func testV2CommandRecoveryReusesExactIdentityAndNeverReposts() async throws {
        let fixture = try Fixture()
        let session = fixture.session()
        let first = try await session.execute(requestID: "command-1", method: "POST",
            path: "/v2/domain/commands", body: command, statusPolls: 0)
        XCTAssertEqual(first, .unknown)
        let recovered = try await session.execute(requestID: "command-1", method: "POST",
            path: "/v2/domain/commands", body: command)
        XCTAssertEqual(recovered, .completed(status: 200, body: ok))
        do {
            _ = try await session.execute(requestID: "command-1", method: "POST",
                path: "/v2/domain/commands", body: changedCommand)
            XCTFail("A changed v2 envelope reused an existing command identity")
        } catch RelaySessionError.operationIdentityConflict { }
        let posts = await fixture.http.mutations
        XCTAssertEqual(posts.count, 1)
        let request = try opened(fixture, posts.first)
        XCTAssertEqual(request["path"] as? String, "/v2/domain/commands")
        let body = try XCTUnwrap(request["body"] as? [String: Any])
        XCTAssertEqual(body["command_id"] as? String, "command-1")
    }

    func testV2DeliveryPathsAreExactAndScoped() {
        XCTAssertTrue(RelaySession.validDomainDeliveryPath(method: "GET", path: "/v2/domain/delivery-attempts?after=0"))
        XCTAssertTrue(RelaySession.validDomainDeliveryPath(method: "POST", path: "/v2/domain/delivery-attempts/42/resolve"))
        for (method, path) in [
            ("GET", "/v2/domain/delivery-attempts?after=0&extra=1"),
            ("GET", "/v2/domain/delivery-attempts?after=-1"),
            ("POST", "/v2/domain/delivery-attempts/0/resolve"),
            ("POST", "/v2/domain/delivery-attempts/42/resolve/extra"),
            ("DELETE", "/v2/domain/delivery-attempts/42/resolve")
        ] {
            XCTAssertFalse(RelaySession.validDomainDeliveryPath(method: method, path: path))
        }
    }

    func testV2ScheduleRoutesMatchMacBodyAndIdentifierRules() {
        for (method, path, body) in [
            ("GET", "/v2/domain/schedules", false),
            ("GET", "/v2/domain/schedules/schedule-1", false),
            ("POST", "/v2/domain/schedules", true),
            ("PUT", "/v2/domain/schedules/schedule-1", true),
            ("DELETE", "/v2/domain/schedules/schedule-1", false),
            ("POST", "/v2/domain/schedules/schedule-1/run", false),
        ] {
            XCTAssertTrue(RelaySession.validDomainSchedulePath(method: method, path: path, hasBody: body), path)
        }
        for (method, path, body) in [
            ("GET", "/v2/domain/schedules/schedule-1/run", false),
            ("POST", "/v2/domain/schedules/schedule-1/run", true),
            ("POST", "/v2/domain/schedules", false),
            ("DELETE", "/v2/domain/schedules/schedule-1", true),
            ("GET", "/v2/domain/schedules/bad%2Fid", false),
            ("GET", "/v2/domain/schedules/id/extra", false),
            ("GET", "/v2/domain/schedules/", false),
        ] {
            XCTAssertFalse(RelaySession.validDomainSchedulePath(method: method, path: path, hasBody: body), path)
        }
    }

    func testV2ScheduleReadIsEphemeralAndCreateRetryReusesSealedOperation() async throws {
        let fixture = try Fixture()
        let session = fixture.session()
        let read = try await session.execute(requestID: "caller-read", method: "GET", path: "/v2/domain/schedules")
        XCTAssertEqual(read, .completed(status: 200, body: ok))
        let readRecord = try await session.recoveryStatus(requestID: "caller-read")
        XCTAssertEqual(readRecord, .notDispatched)

        let body = Data(#"{"title":"Review","text":"Review project","frequency":"daily","hour":9,"minute":0,"revision":0}"#.utf8)
        let first = try await session.execute(requestID: "schedule-create-1", method: "POST",
            path: "/v2/domain/schedules", body: body, statusPolls: 0)
        XCTAssertEqual(first, .unknown)
        let recovered = try await session.execute(requestID: "schedule-create-1", method: "POST",
            path: "/v2/domain/schedules", body: body)
        XCTAssertEqual(recovered, .completed(status: 200, body: ok))
        let sealed = await fixture.http.mutations
        XCTAssertEqual(sealed.count, 2, "one ephemeral read and one create; no replay POST")
        let request = try opened(fixture, sealed.last)
        XCTAssertEqual(request["path"] as? String, "/v2/domain/schedules")
        XCTAssertEqual(request["method"] as? String, "POST")
        XCTAssertEqual((request["body"] as? [String: Any])?["revision"] as? Int, 0)
    }

    func testRestartRefreshesChallengeForStatusWithoutRepostingMutation() async throws {
        let fixture = try Fixture(mode: .restart)
        let result = try await fixture.session().execute(requestID: "operation-1", method: "POST", path: "/v2/domain/commands", body: command)
        XCTAssertEqual(result, .completed(status: 200, body: ok))
        let mutations = await fixture.http.mutations
        let headers = await fixture.http.statusEpochs
        XCTAssertEqual(mutations.count, 1)
        XCTAssertEqual(headers, ["AAAAAAAAAAAAAAAAAAAAAA", "AQEBAQEBAQEBAQEBAQEBAQ"])
    }

    func testContentLongPollUsesEncryptedDaemonCursorAndReservesNoIdentity() async throws {
        let fixture = try Fixture(mode: .overview)
        let session = fixture.session()
        let changed = try await changes(fixture, session, after: 41)
        XCTAssertEqual(changed.revision, 42)
        let posts = await fixture.http.mutations
        XCTAssertEqual(posts.count, 1)
        let request = try opened(fixture, posts.first)
        XCTAssertEqual(request["method"] as? String, "GET")
        XCTAssertEqual(request["path"] as? String, "/v2/domain/changes?data_epoch=epoch&after=41&wait=50")
        XCTAssertNil(request["body"])
        let envelope = try JSONDecoder().decode(EnvelopeV1.self, from: XCTUnwrap(posts.first))
        let recorded = try await session.recoveryStatus(requestID: envelope.requestID)
        XCTAssertEqual(recorded, .notDispatched)
    }

    func testContentLongPollWaitsForRemoteResultAndLogoutSuppressesNextFetch() async throws {
        let fixture = try Fixture(mode: .pendingChange)
        let timer = TimerGate()
        await fixture.http.holdChanges(on: timer)
        let session = fixture.session()
        _ = try await summary(fixture, session)
        let nextRefresh = Task {
            _ = try await self.changes(fixture, session, after: 41)
            return try await self.summary(fixture, session)
        }
        let duration = await timer.started()
        XCTAssertEqual(duration, .seconds(25))
        let beforeRelease = await fixture.http.mutations
        XCTAssertEqual(beforeRelease.count, 2, "One summary and one content poll; no next summary before its response")
        await session.deactivate()
        await timer.release()
        do { _ = try await nextRefresh.value; XCTFail("Logged-out session fetched again") }
        catch is CancellationError { }
        let afterRelease = await fixture.http.mutations
        XCTAssertEqual(afterRelease.count, 2)
    }

    func testFiftySecondContentReadUsesRelayWakeupsWithoutPollingEachSecond() async throws {
        let fixture = try Fixture(mode: .longChange)
        _ = try await changes(fixture, fixture.session(), after: 41)
        let posts = await fixture.http.mutations
        let statusReads = await fixture.http.requestStatusCount
        let changes = await fixture.http.changeQueries
        let elapsed = await fixture.http.simulatedSeconds
        XCTAssertEqual(posts.count, 1)
        XCTAssertEqual(statusReads, 2, "Lost submission receipt and final authenticated response only")
        XCTAssertEqual(changes.map(\.since), [1, 2, 2], "Other jobs and received events advance only the relay cursor")
        XCTAssertEqual(changes.map(\.wait), [25, 25, 25])
        XCTAssertEqual(changes.map(\.epoch), Array(repeating: "AAAAAAAAAAAAAAAAAAAAAA", count: 3))
        XCTAssertEqual(elapsed, 50, "Two server-held intervals; the first response contains immediate unrelated/received events")
    }

    func testReadWakeupRejectsEpochErrorsAndForgedCompletionWithoutReposting() async throws {
        for mode in [FakeHTTP.Mode.changeUnauthorized, .changeConflict, .changeEpoch, .changeForged] {
            let fixture = try Fixture(mode: mode)
            do {
                _ = try await changes(fixture, fixture.session(), after: 41)
                XCTFail("An invalid relay wakeup/completion became content success")
            } catch ServiceAPIError.outcomeUnknown { }
            let posts = await fixture.http.mutations
            XCTAssertEqual(posts.count, 1, "No new encrypted request after authentication/epoch/verification failure")
        }
    }

    func testCancellationDuringRelayWaitDoesNotReadStatusOrPostAgain() async throws {
        let fixture = try Fixture(mode: .pendingChange)
        let gate = TimerGate()
        await fixture.http.holdChanges(on: gate)
        let session = fixture.session()
        _ = try await summary(fixture, session)
        let task = Task { _ = try await self.changes(fixture, session, after: 41) }
        _ = await gate.started()
        let readsBefore = await fixture.http.requestStatusCount
        task.cancel()
        await gate.release()
        do { try await task.value; XCTFail("Cancelled read succeeded") }
        catch is CancellationError { }
        let posts = await fixture.http.mutations
        let readsAfter = await fixture.http.requestStatusCount
        XCTAssertEqual(posts.count, 2, "Summary plus one content read")
        XCTAssertEqual(readsAfter, readsBefore)
    }

    func testReadCancelsDeadlineChildOnSuccessAndBootstrapFailure() async throws {
        for mode in [FakeHTTP.Mode.overview, .bootstrapFailure] {
            let fixture = try Fixture(mode: mode)
            let timer = DeadlineHTTPGate(returnsLate: false)
            let session = fixture.session(readDeadline: { _ in try await timer.wait() })
            do {
                _ = try await changes(fixture, session, after: 41)
                XCTAssertEqual(mode, .overview)
            } catch RelayWireError.unauthorized {
                XCTAssertEqual(mode, .bootstrapFailure, "Bootstrap errors retain their original policy")
            }
            await timer.cancelled()
            let posts = await fixture.http.mutations
            XCTAssertEqual(posts.count, mode == .overview ? 1 : 0)
        }
    }

    func testReadBudgetCancelsInFlightHTTPAndRejectsLateResponsesAtEveryPhase() async throws {
        for phase in ["bootstrap", "status", "changes"] {
            for returnsLate in [false, true] {
                let fixture = try Fixture(mode: phase == "changes" ? .longChange : .overview)
                let timer = TimerGate()
                let http = DeadlineHTTPGate(returnsLate: returnsLate)
                await fixture.http.stall(phase, on: http)
                let session = fixture.session(readDeadline: { deadline in
                    let remaining = ContinuousClock().now.duration(to: deadline)
                    XCTAssertGreaterThan(remaining, .zero)
                    XCTAssertLessThanOrEqual(remaining, .seconds(55))
                    try await timer.wait(remaining)
                })
                let read = Task { _ = try await self.changes(fixture, session, after: 41) }
                await http.started()
                _ = await timer.started()
                await timer.release() // Controlled expiry while HTTP is still suspended.
                await http.cancelled()
                if returnsLate { await http.releaseLateResponse() }
                do { try await read.value; XCTFail("Expired \(phase) accepted a late result") }
                catch ServiceAPIError.outcomeUnknown { }
                let posts = await fixture.http.mutations
                let statusReads = await fixture.http.requestStatusCount
                XCTAssertEqual(posts.count, phase == "bootstrap" ? 0 : 1)
                XCTAssertLessThanOrEqual(statusReads, 1, "Deadline must not authorize another status query")
            }
        }
    }

    func testUncertainMacExecutionAndForgedIdentityCannotBecomeSuccess() async throws {
        for mode in [FakeHTTP.Mode.oversized, .wrongSender, .wrongRequest, .expired] {
            let fixture = try Fixture(mode: mode)
            let session = fixture.session()
            let result = try await session.execute(requestID: "operation-1", method: "POST", path: "/v2/domain/commands", body: command)
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
    let http: FakeHTTP
    let scope = RelayOutboxScope(relay: URL(string: "https://relay.example")!, macID: "mac-test",
                                 deviceID: "phone-test", credentialRevision: "approved-generation-1")
    init(mode: FakeHTTP.Mode = .success) throws {
        http = FakeHTTP(macSigning: macSigning, phoneAgreement: phoneAgreement.publicKey, mode: mode)
    }
    func session(now: @escaping @Sendable () -> UInt64 = { 1000 }, sleep: @escaping @Sendable () async throws -> Void = {},
                 readDeadline: @escaping @Sendable (ContinuousClock.Instant) async throws -> Void = { try await ContinuousClock().sleep(until: $0) }) -> RelaySession {
        return try! RelaySession(scope: scope,
            bearer: { "token" }, transport: http,
            signing: phoneSigning, agreement: phoneAgreement, macSigning: macSigning.publicKey,
            macAgreement: macAgreement.publicKey, now: now, sleep: sleep, waitForReadDeadline: readDeadline)
    }
}

private actor FakeHTTP: ServiceHTTPTransport {
    enum Mode { case bootstrapFailure, success, oversized, rejected, wrongSender, wrongRequest, expired, restart, overview, pendingChange, longChange, changeUnauthorized, changeConflict, changeEpoch, changeForged }
    let macSigning: P256.Signing.PrivateKey
    let phoneAgreement: P256.KeyAgreement.PublicKey
    let mode: Mode
    var mutations: [Data] = []
    var statusEpochs: [String] = []
    var restarted = false
    struct ChangeQuery { let since: UInt64; let epoch: String; let wait: UInt64 }
    var changeQueries: [ChangeQuery] = []
    var simulatedSeconds: UInt64 = 0
    var requestStatusCount = 0
    var completedChange = false
    var stallPhase: String?
    var stallGate: DeadlineHTTPGate?
    func stall(_ phase: String, on gate: DeadlineHTTPGate) { stallPhase = phase; stallGate = gate }
    var changeGate: TimerGate?
    func holdChanges(on gate: TimerGate) { changeGate = gate }
    private var waitsForChange: Bool {
        [.longChange, .changeUnauthorized, .changeConflict, .changeEpoch, .changeForged].contains(mode)
    }
    init(macSigning: P256.Signing.PrivateKey, phoneAgreement: P256.KeyAgreement.PublicKey, mode: Mode) {
        self.macSigning = macSigning; self.phoneAgreement = phoneAgreement; self.mode = mode
    }
    func send(_ request: URLRequest) async throws -> ServiceHTTPResponse {
        let path = request.url?.path ?? ""
        if let stallGate,
           (stallPhase == "bootstrap" && path == "/v1/bootstrap") ||
           (stallPhase == "status" && request.httpMethod == "GET" && path.hasPrefix("/v1/requests/")) ||
           (stallPhase == "changes" && path == "/v1/changes") {
            try await stallGate.wait()
        }
        if request.url?.path == "/v1/bootstrap" {
            if mode == .bootstrapFailure { return ServiceHTTPResponse(status: 401, body: Data()) }
            let epoch = restarted ? "AQEBAQEBAQEBAQEBAQEBAQ" : "AAAAAAAAAAAAAAAAAAAAAA"
            return ServiceHTTPResponse(status: 200, body: try JSONSerialization.data(withJSONObject: ["protocol": 1, "epoch": epoch]))
        }
        if request.httpMethod == "POST" {
            mutations.append(try XCTUnwrap(request.httpBody))
            throw URLError(.timedOut)
        }
        if request.url?.path == "/v1/changes" {
            let items = URLComponents(url: try XCTUnwrap(request.url), resolvingAgainstBaseURL: false)?.queryItems ?? []
            let since = try XCTUnwrap(items.first { $0.name == "since" }?.value.flatMap(UInt64.init))
            let wait = try XCTUnwrap(items.first { $0.name == "wait_s" }?.value.flatMap(UInt64.init))
            let epoch = try XCTUnwrap(items.first { $0.name == "epoch" }?.value)
            changeQueries.append(ChangeQuery(since: since, epoch: epoch, wait: wait))
            let requestID = try JSONDecoder().decode(EnvelopeV1.self, from: XCTUnwrap(mutations.last)).requestID
            if let changeGate { try await changeGate.wait(.seconds(wait)) }
            if mode == .changeUnauthorized { return ServiceHTTPResponse(status: 401, body: Data()) }
            if mode == .changeConflict { return ServiceHTTPResponse(status: 409, body: Data()) }
            let responseEpoch = mode == .changeEpoch ? "AQEBAQEBAQEBAQEBAQEBAQ" : epoch
            let jobs: [[String: Any]]
            if changeQueries.count == 1 {
                jobs = [["request_id": requestID, "status": "received", "revision": 2],
                        ["request_id": "other-job", "status": "completed", "revision": 2]]
            } else if changeQueries.count == 2 {
                simulatedSeconds += wait
                jobs = []
            } else {
                simulatedSeconds += wait
                completedChange = true
                jobs = [["request_id": requestID, "status": "completed", "revision": 3]]
            }
            return ServiceHTTPResponse(status: 200, body: try JSONSerialization.data(withJSONObject:
                ["epoch": responseEpoch, "revision": completedChange ? 3 : 2, "changes": jobs]))
        }
        requestStatusCount += 1
        statusEpochs.append(request.value(forHTTPHeaderField: "x-relay-epoch") ?? "")
        if mode == .restart && !restarted {
            restarted = true
            return ServiceHTTPResponse(status: 401, body: Data())
        }
        let requestID = try JSONDecoder().decode(EnvelopeV1.self, from: XCTUnwrap(mutations.last)).requestID
        if mode == .pendingChange && mutations.count > 1 || waitsForChange && !completedChange {
            return ServiceHTTPResponse(status: 200, body: try JSONSerialization.data(withJSONObject: ["request_id": requestID, "status": "pending", "revision": 1]))
        }
        let payload = mode == .overview || mode == .pendingChange || waitsForChange ? #"{"status":200,"body":\#(changesBody)}"# : mode == .oversized ? #"{"status":502,"error":"response_too_large"}"# : mode == .rejected ? #"{"status":422,"body":{"error":"invalid message"}}"# : #"{"status":200,"body":{"ok":true}}"#
        let envelope = try EnvelopeV1.seal(route: "mac-test", sender: mode == .wrongSender || mode == .changeForged ? "mac-other" : "mac-test",
            recipient: "phone-test", requestID: mode == .wrongRequest ? "operation-other" : requestID, kind: "response",
            expiresAtMilliseconds: mode == .expired ? 999 : 5000, plaintext: Data(payload.utf8),
            senderKey: macSigning, recipientKey: phoneAgreement)
        var json = [String: Any]()
        json["request_id"] = requestID; json["status"] = "completed"; json["revision"] = 4
        json["response"] = try JSONSerialization.jsonObject(with: JSONEncoder().encode(envelope))
        return ServiceHTTPResponse(status: 200, body: try JSONSerialization.data(withJSONObject: json))
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

/// Models both URLSession cancellation and an uncooperative transport returning after cancellation.
private actor DeadlineHTTPGate {
    private let returnsLate: Bool
    private var response: CheckedContinuation<Void, Error>?
    private var startObserver: CheckedContinuation<Void, Never>?
    private var cancelObserver: CheckedContinuation<Void, Never>?
    private var didStart = false
    private var didCancel = false
    init(returnsLate: Bool) { self.returnsLate = returnsLate }
    func wait() async throws {
        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
                if didCancel { continuation.resume(throwing: CancellationError()); return }
                response = continuation
                didStart = true
                startObserver?.resume(); startObserver = nil
            }
        } onCancel: { Task { await self.cancel() } }
    }
    func started() async {
        if didStart { return }
        await withCheckedContinuation { startObserver = $0 }
    }
    func cancelled() async {
        if didCancel { return }
        await withCheckedContinuation { cancelObserver = $0 }
    }
    private func cancel() {
        didCancel = true
        if !returnsLate { response?.resume(throwing: CancellationError()); response = nil }
        cancelObserver?.resume(); cancelObserver = nil
    }
    func releaseLateResponse() { response?.resume(); response = nil }
}
