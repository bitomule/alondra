import AlondraContract
import AlondraFeed
import AlondraFeatures
@testable import AlondraTransport
import ComposableArchitecture
import CryptoKit
import Foundation
import XCTest

private actor ResponseBarrier {
    private var arrived = false
    private var released = false
    private var observers: [CheckedContinuation<Void, Never>] = []
    private var waiting: [CheckedContinuation<Void, Never>] = []

    func hold() async {
        arrived = true
        observers.forEach { $0.resume() }
        observers.removeAll()
        guard !released else { return }
        await withCheckedContinuation { waiting.append($0) }
    }

    func entered() async {
        guard !arrived else { return }
        await withCheckedContinuation { observers.append($0) }
    }

    func release() {
        released = true
        waiting.forEach { $0.resume() }
        waiting.removeAll()
    }
}

// The HTTP peers and their effect journal are synthetic; the client actors, signing,
// envelopes, status reconciliation and action decoding are production implementations.
private actor ControlledRelay: HTTPTransport {
    private let phoneSigning: P256.Signing.PublicKey
    private let phoneAgreement: P256.KeyAgreement.PublicKey
    private let macSigning: P256.Signing.PrivateKey
    private let macAgreement: P256.KeyAgreement.PrivateKey
    private let heldID: String?
    private let responseStatus: Int
    private let uncertain: Bool
    private let barrier: ResponseBarrier
    private var droppedStatuses: Set<String>
    private var replies: [String: Envelope] = [:]
    private var submissions: [String: Int] = [:]
    private var effects: [String: Int] = [:]
    private var statusReads: [String: Int] = [:]
    private let epoch = Data(repeating: 7, count: 16)

    init(phoneSigning: P256.Signing.PublicKey, phoneAgreement: P256.KeyAgreement.PublicKey,
         macSigning: P256.Signing.PrivateKey, macAgreement: P256.KeyAgreement.PrivateKey,
         heldID: String?, barrier: ResponseBarrier, droppedStatuses: Set<String>,
         responseStatus: Int, uncertain: Bool) {
        self.phoneSigning = phoneSigning; self.phoneAgreement = phoneAgreement
        self.macSigning = macSigning; self.macAgreement = macAgreement
        self.heldID = heldID; self.barrier = barrier; self.droppedStatuses = droppedStatuses
        self.responseStatus = responseStatus; self.uncertain = uncertain
    }

    func counts(_ id: String) -> (submissions: Int, effects: Int, statusReads: Int) {
        (submissions[id, default: 0], effects[id, default: 0], statusReads[id, default: 0])
    }

    func send(_ request: URLRequest) async throws -> HTTPReply {
        let url = try XCTUnwrap(request.url)
        if url.path == "/v1/bootstrap" {
            return try json(["protocol": 1, "epoch": Base64URL.encode(epoch)])
        }
        try verify(request)
        if request.httpMethod == "POST", url.path == "/v1/requests" {
            let envelope = try JSONDecoder().decode(Envelope.self, from: try XCTUnwrap(request.httpBody))
            let plain = try envelope.open(senderKey: phoneSigning, recipientKey: macAgreement,
                sender: "phone", recipient: "mac", route: "mac", nowMilliseconds: 1_000)
            let payload = try XCTUnwrap(JSONSerialization.jsonObject(with: plain) as? [String: Any])
            XCTAssertEqual(payload["method"] as? String, "POST")
            XCTAssertEqual(payload["path"] as? String, "/actions")
            let action = try ContractCodec.decode(ActionRequest.self,
                from: JSONSerialization.data(withJSONObject: try XCTUnwrap(payload["body"])))
            XCTAssertEqual(action.commandId, envelope.requestID)
            let id = envelope.requestID
            submissions[id, default: 0] += 1
            if replies[id] == nil {
                effects[id, default: 0] += 1
                let body: [String: Any] = ["protocol": 1, "outcome": "applied",
                    "stamp": "Saved " + (action.input.text ?? ""), "revision": 2]
                var response: [String: Any] = ["status": responseStatus, "body": body]
                if uncertain { response["error"] = "uncertain" }
                replies[id] = try Envelope.seal(route: "mac", sender: "mac", recipient: "phone",
                    requestID: id, kind: "response", expiresAtMilliseconds: 121_000,
                    plaintext: JSONSerialization.data(withJSONObject: response),
                    senderKey: macSigning, recipientKey: phoneAgreement)
            }
            return try json(["request_id": id, "status": "queued", "revision": 1])
        }
        if request.httpMethod == "GET", url.path.hasPrefix("/v1/requests/") {
            let id = url.lastPathComponent
            statusReads[id, default: 0] += 1
            if id == heldID { await barrier.hold() }
            if droppedStatuses.remove(id) != nil { throw URLError(.networkConnectionLost) }
            let response = try XCTUnwrap(replies[id])
            let encoded = try JSONSerialization.jsonObject(with: JSONEncoder().encode(response))
            return try json(["request_id": id, "status": "completed", "revision": 2, "response": encoded])
        }
        throw RelayWireError.invalidEndpoint
    }

    private func verify(_ request: URLRequest) throws {
        let url = try XCTUnwrap(request.url)
        let components = try XCTUnwrap(URLComponents(url: url, resolvingAgainstBaseURL: false))
        let target = components.percentEncodedPath + (components.percentEncodedQuery.map { "?" + $0 } ?? "")
        let timestamp = try XCTUnwrap(request.value(forHTTPHeaderField: "x-device-timestamp-ms").flatMap(UInt64.init))
        let nonce = try Base64URL.decode(try XCTUnwrap(request.value(forHTTPHeaderField: "x-device-nonce")))
        let signature = try P256.Signing.ECDSASignature(rawRepresentation:
            Base64URL.decode(try XCTUnwrap(request.value(forHTTPHeaderField: "x-device-signature"))))
        XCTAssertEqual(request.value(forHTTPHeaderField: "x-device-id"), "phone")
        XCTAssertEqual(request.value(forHTTPHeaderField: "x-relay-epoch"), Base64URL.encode(epoch))
        let message = try RelayHTTPChallenge.message(deviceID: "phone", method: try XCTUnwrap(request.httpMethod),
            target: target, epoch: epoch, body: request.httpBody ?? Data(), timestampMilliseconds: timestamp, nonce: nonce)
        guard phoneSigning.isValidSignature(signature, for: message) else { throw RelayWireError.invalidSignature }
    }

    private func json(_ object: [String: Any]) throws -> HTTPReply {
        HTTPReply(status: 200, body: try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]))
    }
}

private struct ClientFixture: Sendable {
    let barrier = ResponseBarrier()
    let peer: ControlledRelay
    let sender: ViewActionSender

    init(heldID: String? = nil, droppedStatuses: Set<String> = [],
         responseStatus: Int = 200, uncertain: Bool = false) throws {
        let phoneSigning = try P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 1, count: 32))
        let phoneAgreement = try P256.KeyAgreement.PrivateKey(rawRepresentation: Data(repeating: 2, count: 32))
        let macSigning = try P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 3, count: 32))
        let macAgreement = try P256.KeyAgreement.PrivateKey(rawRepresentation: Data(repeating: 4, count: 32))
        peer = ControlledRelay(phoneSigning: phoneSigning.publicKey, phoneAgreement: phoneAgreement.publicKey,
            macSigning: macSigning, macAgreement: macAgreement, heldID: heldID,
            barrier: barrier, droppedStatuses: droppedStatuses,
            responseStatus: responseStatus, uncertain: uncertain)
        let url = URL(string: "https://relay.fixture.invalid")!
        let session = try RelaySession(scope: RelayOutboxScope(relay: url, macID: "mac", deviceID: "phone",
            credentialRevision: "fixture"), bearer: { "synthetic-bearer" }, transport: peer,
            signing: phoneSigning, agreement: phoneAgreement, macSigning: macSigning.publicKey,
            macAgreement: macAgreement.publicKey, now: { 1_000 }, sleep: {})
        sender = ViewActionSender(transport: AlondraAPIClient(serviceURL: url, deviceID: "phone", operations: session))
    }
}

final class ClientConcurrencyTests: XCTestCase {
    private let firstID = "00000000-0000-4000-8000-000000000001"
    private let secondID = "00000000-0000-4000-8000-000000000002"

    private func action(_ id: String, text: String) -> ActionRequest {
        ActionRequest(commandId: id, dataEpoch: "fixture", client: .ios, lang: "en",
            actionId: "answer:question@1", input: ActionInput(text: text))
    }

    func testAnIndependentActionFinishesWhileAnotherAwaitsItsReceipt() async throws {
        let fixture = try ClientFixture(heldID: firstID)
        let request = action(firstID, text: "First")
        let first = Task { try await fixture.sender.send(request) }
        await fixture.barrier.entered()
        let second = try await fixture.sender.send(action(secondID, text: "Second"))
        XCTAssertEqual(second.stamp, "Saved Second")
        let pending = await fixture.sender.unresolvedCommandIDs()
        XCTAssertEqual(pending, [firstID])
        await fixture.barrier.release()
        let answer = try await first.value
        XCTAssertEqual(answer.stamp, "Saved First")
        let firstCounts = await fixture.peer.counts(firstID)
        let secondCounts = await fixture.peer.counts(secondID)
        XCTAssertEqual(firstCounts.effects, 1)
        XCTAssertEqual(secondCounts.effects, 1)
    }

    func testVerifiedUncertainResultsKeepTheIntentWithoutResubmitting() async throws {
        for (status, uncertain) in [(409, true), (503, true), (500, false)] {
            let fixture = try ClientFixture(responseStatus: status, uncertain: uncertain)
            let request = action(firstID, text: "Uncertain")
            do {
                _ = try await fixture.sender.send(request)
                XCTFail("A verified uncertain result must remain unresolved")
            } catch { XCTAssertEqual(error as? OperationError, .outcomeUnknown) }
            let pending = await fixture.sender.unresolvedCommandIDs()
            XCTAssertEqual(pending, [firstID])
            do {
                _ = try await fixture.sender.retry(commandID: firstID)
                XCTFail("An unchanged uncertain receipt must not become a rejection")
            } catch { XCTAssertEqual(error as? OperationError, .outcomeUnknown) }
            let counts = await fixture.peer.counts(firstID)
            XCTAssertEqual(counts.submissions, 1)
            XCTAssertEqual(counts.effects, 1)
            XCTAssertEqual(counts.statusReads, 1)
            let remaining = await fixture.sender.unresolvedCommandIDs()
            XCTAssertEqual(remaining, [firstID])
        }
    }

    func testAnAppliedActionWithALostReceiptRecoversWithoutAnotherSubmission() async throws {
        let fixture = try ClientFixture(droppedStatuses: [firstID])
        do {
            _ = try await fixture.sender.send(action(firstID, text: "First"))
            XCTFail("Losing the result must preserve uncertainty")
        } catch { XCTAssertEqual(error as? OperationError, .outcomeUnknown) }
        let pending = await fixture.sender.unresolvedCommandIDs()
        XCTAssertEqual(pending, [firstID])
        let before = await fixture.peer.counts(firstID)
        XCTAssertEqual(before.effects, 1)
        let answer = try await fixture.sender.retry(commandID: firstID)
        XCTAssertEqual(answer.stamp, "Saved First")
        let after = await fixture.peer.counts(firstID)
        XCTAssertEqual(after.submissions, 1)
        XCTAssertEqual(after.effects, 1)
        XCTAssertEqual(after.statusReads, 2)
        let remaining = await fixture.sender.unresolvedCommandIDs()
        XCTAssertTrue(remaining.isEmpty)
    }
}

final class TasksPageConcurrencyTests: XCTestCase {
    @MainActor
    func testAPageCompletingAfterRefreshCannotRepopulateTheOldRows() async throws {
        let first = try Samples.document("tasks-populated")
        let newer = try Samples.document("tasks-populated") { $0["version"] = "tasks-newer" }
        let barrier = ResponseBarrier()
        var state = TasksFeature.State()
        state.screen.document = first
        state.screen.phase = .loaded
        state.actions.dataEpoch = first.dataEpoch
        state.pagesVersion = first.version
        state.expanded = ["finished"]
        let store = TestStore(initialState: state) { TasksFeature() } withDependencies: {
            $0.viewsClient.load = { _, _ in await barrier.hold(); return .document(first) }
        }
        await store.send(.showMore("finished")) {
            $0.loadingMore = ["finished"]
            $0.pageSequence = 1
            $0.pageLoads = ["finished": 1]
        }
        await barrier.entered()
        await store.send(.foldToggled("finished")) { $0.expanded = [] }
        await store.send(.screen(.loaded(newer))) {
            $0.screen.document = newer
            $0.pagesVersion = nil
            $0.loadingMore = []
            $0.pageLoads = [:]
        }
        await barrier.release()
        await store.receive(\.moreLoaded)
        await store.finish()
        XCTAssertTrue(store.state.pages.isEmpty)
        XCTAssertEqual(store.state.screen.document?.version, newer.version)
    }

    @MainActor
    func testAnOldFailureCannotClearANewerPageLoadOrShowAnError() async throws {
        let first = try Samples.document("tasks-populated")
        let newer = try Samples.document("tasks-populated") { $0["version"] = "tasks-newer" }
        let page = try Samples.document("tasks-populated") { object in
            var sections = object["sections"] as? [[String: Any]] ?? []
            for index in sections.indices where sections[index]["id"] as? String == "finished" {
                sections[index]["items"] = [[
                    "type": "row", "id": "r-9", "icon": "mission", "title": "Newer page", "depth": 0,
                    "struck": false, "subtitle": "Finished", "link": ["kind": "view", "key": "task:t-9"],
                ]]
                sections[index]["more"] = ["key": "tasks?section=finished&after=40", "remaining": 2]
            }
            object["sections"] = sections
        }
        let expectedSection = try XCTUnwrap(page.sections.first { $0.id == "finished" })
        let expectedRows = expectedSection.items.compactMap { item in
            if case let .row(row) = item { return row }
            return nil
        }
        let old = ResponseBarrier()
        let current = ResponseBarrier()
        let calls = LockIsolated(0)
        var state = TasksFeature.State()
        state.screen.document = first
        state.screen.phase = .loaded
        state.actions.dataEpoch = first.dataEpoch
        state.pagesVersion = first.version
        state.expanded = ["finished"]
        let store = TestStore(initialState: state) { TasksFeature() } withDependencies: {
            $0.viewsClient.load = { _, _ in
                let number = calls.withValue { $0 += 1; return $0 }
                if number == 1 {
                    await old.hold()
                    throw ViewsClientError.notInstalled
                }
                await current.hold()
                return .document(page)
            }
        }
        await store.send(.showMore("finished")) {
            $0.loadingMore = ["finished"]
            $0.pageSequence = 1
            $0.pageLoads = ["finished": 1]
        }
        await old.entered()
        await store.send(.foldToggled("finished")) { $0.expanded = [] }
        await store.send(.screen(.loaded(newer))) {
            $0.screen.document = newer
            $0.pagesVersion = nil
            $0.loadingMore = []
            $0.pageLoads = [:]
        }
        await store.send(.foldToggled("finished")) { $0.expanded = ["finished"] }
        await store.receive(\.showMore) {
            $0.loadingMore = ["finished"]
            $0.pageSequence = 2
            $0.pageLoads = ["finished": 2]
        }
        await current.entered()
        await old.release()
        await store.receive(\.moreFailed)
        XCTAssertEqual(store.state.loadingMore, ["finished"])
        await current.release()
        await store.receive(\.moreLoaded) {
            $0.loadingMore = []
            $0.pageLoads = [:]
            $0.pages = ["finished": TasksPage(rows: expectedRows, more: expectedSection.more)]
            $0.pagesVersion = newer.version
        }
        await store.finish()
        XCTAssertEqual(store.state.board?.finished.requests.map(\.id), ["r-3", "r-4", "r-9"])
    }

}
