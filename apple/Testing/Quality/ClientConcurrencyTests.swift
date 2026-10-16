import AlondraContract
import AlondraFeed
import AlondraFeatures
@testable import AlondraTransport
import AlondraTestSupport
import ComposableArchitecture
import CryptoKit
import Foundation
import Testing
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

// The Mac's direct port and its effect journal are synthetic; the client actors, envelopes,
// sealed result lookups and action decoding are production implementations.
private actor ControlledMac: HTTPTransport {
    nonisolated let identity: DirectMacIdentity
    private let phoneSigning: P256.Signing.PublicKey
    private let phoneAgreement: P256.KeyAgreement.PublicKey
    private let macSigning: P256.Signing.PrivateKey
    private let macAgreement: P256.KeyAgreement.PrivateKey
    private let heldID: String?
    private let responseStatus: Int
    private let uncertain: Bool
    private let barrier: ResponseBarrier
    private var lostAnswers: [String: Int]
    private var replies: [String: Envelope] = [:]
    private var originals: [String: Envelope] = [:]
    private var resultReads: [String: Int] = [:]
    private var submissions: [String: Int] = [:]
    private var effects: [String: Int] = [:]

    init(phoneSigning: P256.Signing.PublicKey, phoneAgreement: P256.KeyAgreement.PublicKey,
         macSigning: P256.Signing.PrivateKey, macAgreement: P256.KeyAgreement.PrivateKey,
         heldID: String?, barrier: ResponseBarrier, lostAnswers: [String: Int],
         responseStatus: Int, uncertain: Bool) {
        self.phoneSigning = phoneSigning; self.phoneAgreement = phoneAgreement
        self.macSigning = macSigning; self.macAgreement = macAgreement
        self.heldID = heldID; self.barrier = barrier; self.lostAnswers = lostAnswers
        self.responseStatus = responseStatus; self.uncertain = uncertain
        identity = DirectMacIdentity(macID: "mac-1", name: "Studio",
            signingKey: Base64URL.encode(macSigning.publicKey.x963Representation),
            agreementKey: Base64URL.encode(macAgreement.publicKey.x963Representation))
    }

    func counts(_ id: String) -> (submissions: Int, effects: Int, resultReads: Int) {
        (submissions[id, default: 0], effects[id, default: 0], resultReads[id, default: 0])
    }

    func send(_ request: URLRequest) async throws -> HTTPReply {
        let url = try XCTUnwrap(request.url)
        if url.path == "/direct/hello" { return HTTPReply(status: 200, body: try JSONEncoder().encode(identity)) }
        XCTAssertEqual(url.path, "/direct/request")
        let envelope = try JSONDecoder().decode(Envelope.self, from: try XCTUnwrap(request.httpBody))
        let plain = try envelope.open(senderKey: phoneSigning, recipientKey: macAgreement,
            sender: "phone", recipient: "mac-1", route: "mac-1", nowMilliseconds: 1_000)
        let payload = try XCTUnwrap(JSONSerialization.jsonObject(with: plain) as? [String: Any])
        if payload["method"] as? String == "GET" {
            XCTAssertNil(payload["body"], "recovery never carries a mutation body")
            let path = try XCTUnwrap(payload["path"] as? String)
            let components = try XCTUnwrap(URLComponents(string: path))
            let id = String(components.path.dropFirst("/operation-results/".count))
            XCTAssertEqual(components.path, "/operation-results/" + id)
            XCTAssertTrue(envelope.requestID.hasPrefix("read-"))
            XCTAssertNotEqual(envelope.requestID, id, "the attestation has its own fresh read identity")
            let original = try XCTUnwrap(originals[id])
            let digest = try original.recoveryDigest()
            XCTAssertEqual(components.queryItems?.first(where: { $0.name == "digest" })?.value, digest)
            XCTAssertEqual(components.queryItems?.first(where: { $0.name == "data_epoch" })?.value, "fixture")
            resultReads[id, default: 0] += 1
            let response = try JSONSerialization.jsonObject(with: JSONEncoder().encode(try XCTUnwrap(replies[id])))
            let body: [String: Any] = ["request_id": id, "digest": digest, "data_epoch": "fixture", "response": response]
            return try sealed(["status": 200, "body": body], to: envelope.requestID)
        }
        XCTAssertEqual(payload["method"] as? String, "POST")
        XCTAssertEqual(payload["path"] as? String, "/actions")
        let action = try ContractCodec.decode(ActionRequest.self,
            from: JSONSerialization.data(withJSONObject: try XCTUnwrap(payload["body"])))
        XCTAssertEqual(action.commandId, envelope.requestID)
        let id = envelope.requestID
        submissions[id, default: 0] += 1
        if replies[id] == nil {
            originals[id] = envelope
            effects[id, default: 0] += 1
            let body: [String: Any] = ["protocol": 1, "outcome": "applied",
                "stamp": "Saved " + (action.input.text ?? ""), "revision": 2]
            var response: [String: Any] = ["status": responseStatus, "body": body]
            if uncertain { response["error"] = "uncertain" }
            replies[id] = try Envelope.seal(route: "mac-1", sender: "mac-1", recipient: "phone",
                requestID: id, kind: "response", expiresAtMilliseconds: 121_000,
                plaintext: JSONSerialization.data(withJSONObject: response),
                senderKey: macSigning, recipientKey: phoneAgreement)
        }
        if id == heldID { await barrier.hold() }
        if let left = lostAnswers[id], left > 0 {
            lostAnswers[id] = left - 1
            throw URLError(.networkConnectionLost)
        }
        return HTTPReply(status: 200, body: try JSONEncoder().encode(try XCTUnwrap(replies[id])))
    }

    private func sealed(_ answer: [String: Any], to requestID: String) throws -> HTTPReply {
        HTTPReply(status: 200, body: try JSONEncoder().encode(Envelope.seal(route: "mac-1", sender: "mac-1", recipient: "phone",
            requestID: requestID, kind: "response", expiresAtMilliseconds: 121_000,
            plaintext: JSONSerialization.data(withJSONObject: answer), senderKey: macSigning, recipientKey: phoneAgreement)))
    }
}

private struct ClientFixture: Sendable {
    let barrier = ResponseBarrier()
    let peer: ControlledMac
    let sender: ViewActionSender

    init(heldID: String? = nil, lostAnswers: [String: Int] = [:],
         responseStatus: Int = 200, uncertain: Bool = false) throws {
        let phoneSigning = try P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 1, count: 32))
        let phoneAgreement = try P256.KeyAgreement.PrivateKey(rawRepresentation: Data(repeating: 2, count: 32))
        let macSigning = try P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 3, count: 32))
        let macAgreement = try P256.KeyAgreement.PrivateKey(rawRepresentation: Data(repeating: 4, count: 32))
        peer = ControlledMac(phoneSigning: phoneSigning.publicKey, phoneAgreement: phoneAgreement.publicKey,
            macSigning: macSigning, macAgreement: macAgreement, heldID: heldID,
            barrier: barrier, lostAnswers: lostAnswers,
            responseStatus: responseStatus, uncertain: uncertain)
        let url = URL(string: "alondra-direct://mac-1")!
        let http = DirectHTTP(transport: peer)
        let router = DirectRouter(pinned: peer.identity, saved: [DirectEndpoint(host: "192.168.1.5", port: 47810, kind: .localNetwork)],
                                  http: http, network: { "home" })
        let journal = try EnvelopeJournal(scope: EnvelopeJournalScope(relay: url, macID: "mac-1", deviceID: "phone",
            credentialRevision: "fixture"), directory: TestFiles.directory())
        let session = DirectSession(macID: "mac-1", deviceID: "phone", signing: phoneSigning, agreement: phoneAgreement,
            macSigning: macSigning.publicKey, macAgreement: macAgreement.publicKey, router: router, http: http,
            journal: journal, now: { 1_000 }, pause: { _ in })
        sender = ViewActionSender(transport: AlondraAPIClient(serviceURL: url, deviceID: "phone", operations: session))
    }
}

@Suite(.temporaryFiles)
struct ClientConcurrencyTests {
    private let firstID = "00000000-0000-4000-8000-000000000001"
    private let secondID = "00000000-0000-4000-8000-000000000002"

    private func action(_ id: String, text: String) -> ActionRequest {
        ActionRequest(commandId: id, dataEpoch: "fixture", client: .ios, lang: "en",
            actionId: "answer:question@1", input: ActionInput(text: text))
    }

    @Test func anIndependentActionFinishesWhileAnotherAwaitsItsAnswer() async throws {
        let fixture = try ClientFixture(heldID: firstID)
        let request = action(firstID, text: "First")
        let sender = fixture.sender
        let first = Task { try await sender.send(request) }
        await fixture.barrier.entered()
        let second = try await fixture.sender.send(action(secondID, text: "Second"))
        #expect(second.stamp == "Saved Second")
        #expect(await fixture.sender.unresolvedCommandIDs() == [firstID])
        await fixture.barrier.release()
        #expect(try await first.value.stamp == "Saved First")
        #expect(await fixture.peer.counts(firstID).effects == 1)
        #expect(await fixture.peer.counts(secondID).effects == 1)
    }

    @Test func verifiedUncertainResultsKeepTheIntentWithoutResubmitting() async throws {
        for (status, uncertain) in [(409, true), (503, true), (500, false)] {
            let fixture = try ClientFixture(responseStatus: status, uncertain: uncertain)
            let request = action(firstID, text: "Uncertain")
            await #expect(throws: OperationError.outcomeUnknown, "a verified uncertain result must remain unresolved") {
                try await fixture.sender.send(request)
            }
            #expect(await fixture.sender.unresolvedCommandIDs() == [firstID])
            await #expect(throws: OperationError.outcomeUnknown, "an unchanged uncertain answer must not become a rejection") {
                try await fixture.sender.retry(commandID: firstID)
            }
            let counts = await fixture.peer.counts(firstID)
            #expect(counts.submissions == 1)
            #expect(counts.effects == 1)
            #expect(counts.resultReads == 1)
            #expect(await fixture.sender.unresolvedCommandIDs() == [firstID])
        }
    }

    @Test func aPassingLossOfTheAnswerRecoversByItselfAsTheSameBytes() async throws {
        let fixture = try ClientFixture(lostAnswers: [firstID: 1])
        let answer = try await fixture.sender.send(action(firstID, text: "First"))
        #expect(answer.stamp == "Saved First")
        let counts = await fixture.peer.counts(firstID)
        #expect(counts.submissions == 2)
        #expect(counts.effects == 1)
        #expect(await fixture.sender.unresolvedCommandIDs().isEmpty)
    }

    @Test func anAppliedActionWhoseAnswersWereAllLostRecoversWithoutRunningAgain() async throws {
        let fixture = try ClientFixture(lostAnswers: [firstID: 10])
        await #expect(throws: OperationError.outcomeUnknown, "losing the answer must preserve uncertainty") {
            try await fixture.sender.send(action(firstID, text: "First"))
        }
        #expect(await fixture.sender.unresolvedCommandIDs() == [firstID])
        let before = await fixture.peer.counts(firstID)
        #expect(before.effects == 1)
        #expect(before.submissions == 4, "every try of the send lost its answer")
        let answer = try await fixture.sender.retry(commandID: firstID)
        #expect(answer.stamp == "Saved First")
        let after = await fixture.peer.counts(firstID)
        #expect(after.submissions == 4)
        #expect(after.effects == 1)
        #expect(after.resultReads == 1)
        #expect(await fixture.sender.unresolvedCommandIDs().isEmpty)
    }
}

@Reducer
private struct ReplaceableTasks {
    @ObservableState
    struct State: Equatable { var tasks = TasksFeature.State() }
    enum Action { case tasks(TasksFeature.Action); case replace(TasksFeature.State) }
    var body: some ReducerOf<Self> {
        Scope(state: \.tasks, action: \.tasks) { TasksFeature() }
        Reduce { state, action in
            if case let .replace(tasks) = action { state.tasks = tasks }
            return .none
        }
    }
}

final class TasksPageConcurrencyTests: XCTestCase {
    @MainActor
    func testReplacingTheChildCannotReuseAnOldPagesRequestIdentity() async throws {
        let first = try Samples.document("tasks-populated")
        let newer = try Samples.document("tasks-populated") { $0["version"] = "tasks-newer" }
        func showing(_ document: ViewDocument) -> TasksFeature.State {
            var state = TasksFeature.State()
            state.screen.document = document
            state.screen.phase = .loaded
            state.actions.dataEpoch = document.dataEpoch
            state.pagesVersion = document.version
            state.expanded = ["finished"]
            return state
        }
        let old = ResponseBarrier()
        let current = ResponseBarrier()
        let calls = LockIsolated(0)
        let store = TestStore(initialState: ReplaceableTasks.State(tasks: showing(first))) {
            ReplaceableTasks()
        } withDependencies: {
            $0.uuid = .incrementing
            $0.viewsClient.load = { _, _ in
                let call = calls.withValue { $0 += 1; return $0 }
                if call == 1 { await old.hold(); return .document(first) }
                await current.hold()
                return .document(newer)
            }
        }
        await store.send(.tasks(.showMore("finished"))) {
            $0.tasks.loadingMore = ["finished"]
            $0.tasks.pageLoads = ["finished": UUID(uuidString: "00000000-0000-0000-0000-000000000000")!]
        }
        await old.entered()
        await store.send(.replace(showing(newer))) { $0.tasks = showing(newer) }
        await store.send(.tasks(.showMore("finished"))) {
            $0.tasks.loadingMore = ["finished"]
            $0.tasks.pageLoads = ["finished": UUID(uuidString: "00000000-0000-0000-0000-000000000001")!]
        }
        await current.entered()
        await old.release()
        await store.receive(\.tasks.moreLoaded)
        XCTAssertTrue(store.state.tasks.pages.isEmpty)
        XCTAssertEqual(store.state.tasks.loadingMore, ["finished"])
        await current.release()
        let section = try XCTUnwrap(newer.sections.first { $0.id == "finished" })
        let rows = section.items.compactMap { item in
            if case let .row(row) = item { return row }
            return nil
        }
        await store.receive(\.tasks.moreLoaded) {
            $0.tasks.loadingMore = []
            $0.tasks.pageLoads = [:]
            $0.tasks.pages = ["finished": TasksPage(rows: rows, more: section.more)]
        }
        await store.finish()
    }

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
            $0.uuid = .incrementing
            $0.viewsClient.load = { _, _ in await barrier.hold(); return .document(first) }
        }
        await store.send(.showMore("finished")) {
            $0.loadingMore = ["finished"]
            $0.pageLoads = ["finished": UUID(uuidString: "00000000-0000-0000-0000-000000000000")!]
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
            $0.uuid = .incrementing
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
            $0.pageLoads = ["finished": UUID(uuidString: "00000000-0000-0000-0000-000000000000")!]
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
            $0.pageLoads = ["finished": UUID(uuidString: "00000000-0000-0000-0000-000000000001")!]
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
