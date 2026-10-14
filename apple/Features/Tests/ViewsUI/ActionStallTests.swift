import AlondraContract
import AlondraFeatures
import AlondraFeed
import AlondraTransport
import ComposableArchitecture
import Foundation
import XCTest

/// A Mac that never answers: preparation waits until the person asks to retry.
private actor HungMac {
    private var waiting: [CheckedContinuation<ActionRequest, Never>] = []
    private var released: ActionRequest?
    private(set) var retries = 0
    private(set) var prepared: [ActionRequest] = []
    let answer: ActionRequest

    init(answer: ActionRequest) { self.answer = answer }

    func prepare(_ request: ActionRequest) async -> ActionRequest {
        prepared.append(request)
        if let released { return released }
        return await withCheckedContinuation { waiting.append($0) }
    }

    func retryNow() {
        retries += 1
        released = answer
        for continuation in waiting { continuation.resume(returning: answer) }
        waiting.removeAll()
    }
}

@MainActor
final class ActionStallTests: XCTestCase {
    private let draftID = "00000000-0000-4000-8000-0000000000c1"

    private func state() throws -> ActionFeature.State {
        var state = ActionFeature.State(dataEpoch: "epoch", client: .ios, lang: "en")
        state.action = try ContractCodec.decode(AlondraContract.Action.self, from: Data(#"{"id":"reply:q-1@1","label":"Reply","style":"primary","presentation":"button","input":{"kind":"text","required":true,"multiline":true},"attachments":false,"enabled":true,"resolves_card":false}"#.utf8))
        state.text = "Keep the written reply"
        return state
    }

    private func request(_ id: String) -> ActionRequest {
        ActionRequest(commandId: id, dataEpoch: "epoch", client: .ios, lang: "en",
            actionId: "reply:q-1@1", input: ActionInput(text: "Keep the written reply"), resolvesCard: false)
    }

    private func reply() throws -> ActionResponse {
        try ContractCodec.decode(ActionResponse.self, from: Data(#"{"protocol":1,"outcome":"applied","stamp":"Saved","revision":2}"#.utf8))
    }

    func testAnAnswerTheMacNeverTakesSaysSoOnTheCardAndARetryDeliversIt() async throws {
        let draft = request(draftID)
        let canonical = try request(XCTUnwrap(ActionOperationID(dataEpoch: "epoch", generation: 1, sequence: 1, nonce: draftID)).rawValue)
        let mac = HungMac(answer: canonical)
        let answer = try reply()
        let initial = try state()
        let store = TestStore(initialState: initial) { ActionFeature() } withDependencies: {
            $0.uuid = .constant(UUID(uuidString: draftID)!)
            $0.viewsClient.prepare = { await mac.prepare($0) }
            $0.viewsClient.perform = { _ in answer }
            $0.viewsClient.retryNow = { await mac.retryNow() }
            $0.viewsClient.stallAfter = { .milliseconds(20) }
        }

        await store.send(.submit) { $0.pending = draft; $0.inFlight = true; $0.sends = 1 }
        await store.receive(\.sending, draft)
        await store.receive(\.stalled) { $0.stalled = true }
        XCTAssertEqual(store.state.stallNotice, "Couldn’t send · Retrying…")
        XCTAssertNil(store.state.stamp)

        await store.send(.submit)
        await store.receive(\.retryNow)
        let prepared = await mac.prepared
        XCTAssertEqual(prepared, [draft], "tapping again while it retries never makes a second answer")

        await store.receive(\.prepared) { $0.pending = canonical; $0.stamp = "Sent" }
        await store.receive(\.sent, canonical)
        await store.receive(\.settled)
        await store.receive(\.answered)
        await store.receive(\.response) {
            $0.pending = nil; $0.inFlight = false; $0.stalled = false; $0.stamp = "Saved"
        }
        await store.receive(\.delegate.completed)
        XCTAssertNil(store.state.stallNotice)
        let retries = await mac.retries
        XCTAssertEqual(retries, 1)
        await store.finish()
    }

    func testAMacThatAnswersInTimeNeverShowsTheNotice() async throws {
        let draft = request(draftID)
        let answer = try reply()
        let initial = try state()
        let store = TestStore(initialState: initial) { ActionFeature() } withDependencies: {
            $0.uuid = .constant(UUID(uuidString: draftID)!)
            $0.viewsClient.prepare = { $0 }
            $0.viewsClient.perform = { _ in answer }
            $0.viewsClient.stallAfter = { .seconds(60) }
        }

        await store.send(.submit) { $0.pending = draft; $0.inFlight = true; $0.sends = 1 }
        await store.receive(\.sending, draft)
        await store.receive(\.prepared) { $0.stamp = "Sent" }
        await store.receive(\.sent, draft)
        await store.receive(\.settled)
        await store.receive(\.answered)
        await store.receive(\.response) { $0.pending = nil; $0.inFlight = false; $0.stamp = "Saved" }
        await store.receive(\.delegate.completed)
        XCTAssertFalse(store.state.stalled)
        await store.finish()
    }
}
