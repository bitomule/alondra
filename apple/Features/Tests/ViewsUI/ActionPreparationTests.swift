import AlondraContract
import AlondraFeatures
import AlondraFeed
import AlondraTransport
import ComposableArchitecture
import Foundation
import XCTest

private actor ActionPreparationGate {
    let starts: AsyncStream<ActionRequest>
    let cancellations: AsyncStream<Void>
    private let started: AsyncStream<ActionRequest>.Continuation
    private let cancelled: AsyncStream<Void>.Continuation
    private var held: [CheckedContinuation<ActionRequest, any Error>] = []
    private var releasedRequest: ActionRequest?
    private(set) var requests: [ActionRequest] = []

    init() {
        let starts = AsyncStream<ActionRequest>.makeStream()
        self.starts = starts.stream; started = starts.continuation
        let cancellations = AsyncStream<Void>.makeStream()
        self.cancellations = cancellations.stream; cancelled = cancellations.continuation
    }

    func prepare(_ request: ActionRequest) async throws -> ActionRequest {
        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                if Task.isCancelled {
                    continuation.resume(throwing: CancellationError())
                    cancelled.yield(())
                } else {
                    requests.append(request)
                    if let releasedRequest { continuation.resume(returning: releasedRequest) }
                    else { held.append(continuation) }
                    started.yield(request)
                }
            }
        } onCancel: {
            Task { await self.cancel() }
        }
    }

    func release(_ request: ActionRequest) {
        releasedRequest = request
        let waiting = held; held.removeAll()
        for continuation in waiting { continuation.resume(returning: request) }
    }

    private func cancel() {
        let waiting = held; held.removeAll()
        for continuation in waiting { continuation.resume(throwing: CancellationError()) }
        cancelled.yield(())
    }
}

private actor PreparedActionPeer {
    private(set) var requests: [ActionRequest] = []
    let reply: ActionResponse
    init(_ reply: ActionResponse) { self.reply = reply }
    func perform(_ request: ActionRequest) -> ActionResponse {
        requests.append(request)
        return reply
    }
}

@Reducer
private struct PreparationEpochHarness {
    @ObservableState
    struct State: Equatable {
        var action: ActionFeature.State
    }
    enum Action {
        case epochChanged(String)
        case action(ActionFeature.Action)
    }
    var body: some ReducerOf<Self> {
        Scope(state: \.action, action: \.action) { ActionFeature() }
        Reduce { state, action in
            if case let .epochChanged(epoch) = action { state.action.dataEpoch = epoch }
            return .none
        }
    }
}

@MainActor
final class ActionPreparationTests: XCTestCase {
    private let draftID = "00000000-0000-4000-8000-000000000001"
    private let originalNonce = "00000000-0000-4000-8000-000000000002"

    private func state(handsOff: Bool = false) throws -> ActionFeature.State {
        var state = ActionFeature.State(dataEpoch: "epoch", client: .ios, lang: "en")
        state.action = try ContractCodec.decode(AlondraContract.Action.self, from: Data(#"{"id":"reply:q-1@1","label":"Reply","style":"primary","presentation":"button","input":{"kind":"text","required":true,"multiline":true},"attachments":false,"enabled":true,"resolves_card":false}"#.utf8))
        state.text = "Keep the written reply"
        state.handsOff = handsOff
        return state
    }

    private func request(_ id: String) -> ActionRequest {
        ActionRequest(commandId: id, dataEpoch: "epoch", client: .ios, lang: "en",
            actionId: "reply:q-1@1", input: ActionInput(text: "Keep the written reply"), resolvesCard: false)
    }

    private func canonical(_ nonce: String) throws -> ActionRequest {
        let identity = try XCTUnwrap(ActionOperationID(dataEpoch: "epoch", generation: 7, sequence: 11, nonce: nonce))
        return request(identity.rawValue)
    }

    private func reply() throws -> ActionResponse {
        try ContractCodec.decode(ActionResponse.self, from: Data(#"{"protocol":1,"outcome":"applied","stamp":"Saved original answer","revision":2}"#.utf8))
    }

    func testTheCanonicalIntentEntersStateBeforeSentAndFirstDelivery() async throws {
        let gate = ActionPreparationGate()
        var starts = gate.starts.makeAsyncIterator()
        let peer = PreparedActionPeer(try reply())
        let draft = request(draftID)
        let canonical = try canonical(draftID)
        let initial = try state()
        let store = TestStore(initialState: initial) { ActionFeature() } withDependencies: {
            $0.uuid = .constant(UUID(uuidString: draftID)!)
            $0.viewsClient.prepare = { try await gate.prepare($0) }
            $0.viewsClient.perform = { await peer.perform($0) }
        }
        await store.send(.submit) {
            $0.pending = draft; $0.inFlight = true; $0.sends = 1
        }
        let started = await starts.next()
        XCTAssertEqual(started, draft)
        let beforeDispatch = await peer.requests
        XCTAssertTrue(beforeDispatch.isEmpty, "preparation cannot dispatch the action")
        XCTAssertEqual(store.state.pending, draft)
        XCTAssertEqual(store.state.text, "Keep the written reply")
        XCTAssertNil(store.state.stamp, "a held preparation cannot claim that the draft was sent")
        await gate.release(canonical)
        await store.receive(\.prepared) { $0.pending = canonical; $0.stamp = "Sent" }
        XCTAssertEqual(store.state.pending?.commandId, canonical.commandId)
        XCTAssertEqual(store.state.stamp, "Sent", "only the canonical prepared intent can announce that it was sent")
        await store.receive(\.sent, canonical)
        await store.receive(\.settled)
        await store.receive(\.answered)
        await store.receive(\.response) {
            $0.pending = nil; $0.inFlight = false; $0.stamp = "Saved original answer"
        }
        await store.receive(\.delegate.completed, "Action completed")
        await store.finish()
        let dispatched = await peer.requests
        XCTAssertEqual(dispatched, [canonical])
    }

    func testAnAdoptedOldPendingIdentityIsHandedOffInsteadOfTheNewDraftUUID() async throws {
        let gate = ActionPreparationGate()
        var starts = gate.starts.makeAsyncIterator()
        let draft = request(draftID)
        let original = try canonical(originalNonce)
        let initial = try state(handsOff: true)
        let store = TestStore(initialState: initial) { ActionFeature() } withDependencies: {
            $0.uuid = .constant(UUID(uuidString: draftID)!)
            $0.viewsClient.prepare = { try await gate.prepare($0) }
            $0.viewsClient.perform = { _ in XCTFail("handoff preparation must not deliver the action itself"); throw CancellationError() }
        }
        await store.send(.submit) {
            $0.pending = draft; $0.inFlight = true; $0.sends = 1
        }
        let started = await starts.next()
        XCTAssertEqual(started, draft)
        await gate.release(original)
        await store.receive(\.prepared) { $0.pending = nil; $0.inFlight = false; $0.stamp = "Sent" }
        await store.receive(\.sent, original)
        XCTAssertEqual(store.state.text, draft.input.text)
        await store.finish()
    }

    func testAnEpochChangeDuringPreparationCannotEmitAnOldSentAction() async throws {
        let gate = ActionPreparationGate()
        var starts = gate.starts.makeAsyncIterator()
        let draft = request(draftID)
        let canonical = try canonical(draftID)
        let initial = PreparationEpochHarness.State(action: try state(handsOff: true))
        let store = TestStore(initialState: initial) {
            PreparationEpochHarness()
        } withDependencies: {
            $0.uuid = .constant(UUID(uuidString: draftID)!)
            $0.viewsClient.prepare = { try await gate.prepare($0) }
            $0.viewsClient.perform = { _ in XCTFail("an obsolete preparation cannot dispatch"); throw CancellationError() }
        }
        await store.send(.action(.submit)) {
            $0.action.pending = draft; $0.action.inFlight = true; $0.action.sends = 1
        }
        let started = await starts.next()
        XCTAssertEqual(started, draft)
        await store.send(.epochChanged("new-epoch")) { $0.action.dataEpoch = "new-epoch" }
        await gate.release(canonical)
        await store.receive(\.action.prepared)
        XCTAssertNil(store.state.action.pending)
        XCTAssertFalse(store.state.action.inFlight)
        XCTAssertEqual(store.state.action.text, "Keep the written reply")
        await store.finish()
    }

    func testDismissingDuringPreparationCannotHandOffItsLateResult() async throws {
        let gate = ActionPreparationGate()
        var starts = gate.starts.makeAsyncIterator()
        let draft = request(draftID)
        let canonical = try canonical(draftID)
        let initial = try state(handsOff: true)
        let store = TestStore(initialState: initial) { ActionFeature() } withDependencies: {
            $0.uuid = .constant(UUID(uuidString: draftID)!)
            $0.viewsClient.prepare = { try await gate.prepare($0) }
            $0.viewsClient.perform = { _ in XCTFail("a dismissed preparation cannot dispatch"); throw CancellationError() }
        }
        await store.send(.submit) {
            $0.pending = draft; $0.inFlight = true; $0.sends = 1
        }
        let started = await starts.next()
        XCTAssertEqual(started, draft)
        await store.send(.dismiss) {
            $0.action = nil; $0.stamp = nil; $0.pending = nil; $0.inFlight = false
        }
        await gate.release(canonical)
        await store.receive(\.prepared)
        XCTAssertNil(store.state.action)
        XCTAssertEqual(store.state.text, "Keep the written reply")
        await store.finish()
    }

    func testCancellingPreparationCannotEmitSentOrDispatch() async throws {
        let gate = ActionPreparationGate()
        var starts = gate.starts.makeAsyncIterator()
        var cancellations = gate.cancellations.makeAsyncIterator()
        let draft = request(draftID)
        let initial = try state(handsOff: true)
        let store = TestStore(initialState: initial) { ActionFeature() } withDependencies: {
            $0.uuid = .constant(UUID(uuidString: draftID)!)
            $0.viewsClient.prepare = { try await gate.prepare($0) }
            $0.viewsClient.perform = { _ in XCTFail("a cancelled preparation cannot dispatch"); throw CancellationError() }
        }
        let preparation = await store.send(.submit) {
            $0.pending = draft; $0.inFlight = true; $0.sends = 1
        }
        let started = await starts.next()
        XCTAssertEqual(started, draft)
        let cancellation = Task { await preparation.cancel() }
        let cancelled: Void? = await cancellations.next()
        XCTAssertNotNil(cancelled)
        await cancellation.value
        XCTAssertEqual(store.state.pending, draft)
        XCTAssertEqual(store.state.text, "Keep the written reply")
        await store.finish()
    }
    func testFailedPreparationKeepsTheWrittenReplyWithoutClaimingItMayHaveArrived() async throws {
        for error in [ViewActionIntentError.capacityExceeded, .unreadableStore] {
            let initial = try state()
            let draft = request(draftID)
            let message = String(localized: "Alondra couldn’t prepare this request.")
            let store = TestStore(initialState: initial) { ActionFeature() } withDependencies: {
                $0.uuid = .constant(UUID(uuidString: draftID)!)
                $0.viewsClient.prepare = { _ in throw error }
                $0.viewsClient.perform = { _ in
                    XCTFail("an action that failed preparation cannot dispatch")
                    throw CancellationError()
                }
            }
            await store.send(.submit) {
                $0.pending = draft; $0.inFlight = true; $0.sends = 1
            }
            await store.receive(\.undelivered) {
                $0.pending = nil; $0.inFlight = false; $0.stamp = nil
                $0.error = message
            }
            await store.receive(\.delegate.failed, message)
            XCTAssertNil(store.state.pending)
            XCTAssertNil(store.state.unsure)
            XCTAssertFalse(store.state.inFlight)
            XCTAssertEqual(store.state.text, draft.input.text)
            XCTAssertEqual(store.state.error, message)
            await store.finish()
        }
    }

    func testPreparationKeepsTheSpecificApprovalAndConflictMessages() {
        for error in [APIError.unauthorized, .forbidden(kind: nil, detail: nil, reasons: [])] {
            XCTAssertEqual(SendFailure(preparation: error),
                .answered(String(localized: "This device is no longer approved. Approve it again from the Mac.")))
        }
        XCTAssertEqual(SendFailure(preparation: APIError.conflict(kind: nil, detail: nil, reasons: [])),
            .answered(String(localized: "This changed while you were looking. It’s updated now; try again.")))
    }

    func testAnotherPresentationCannotReplaceTheDraftWhilePreparationIsHeld() async throws {
        let other = try ContractCodec.decode(AlondraContract.Action.self, from: Data(#"{"id":"reply:q-2@1","label":"Another reply","style":"primary","presentation":"button","input":{"kind":"text","required":true,"multiline":true},"attachments":false,"enabled":true,"resolves_card":false}"#.utf8))
        let presentations = [ActionFeature.Action.present(other),
            .presentForm(other, ["text": .string("Replace the first draft")], requiresAttachments: true)]
        for presentation in presentations {
            let gate = ActionPreparationGate()
            var starts = gate.starts.makeAsyncIterator()
            let draft = request(draftID)
            let canonical = try canonical(draftID)
            let initial = try state(handsOff: true)
            let store = TestStore(initialState: initial) { ActionFeature() } withDependencies: {
                $0.uuid = .constant(UUID(uuidString: draftID)!)
                $0.viewsClient.prepare = { try await gate.prepare($0) }
                $0.viewsClient.perform = { _ in XCTFail("handoff cannot dispatch in the child"); throw CancellationError() }
            }
            await store.send(.submit) { $0.pending = draft; $0.inFlight = true; $0.sends = 1 }
            let started = await starts.next()
            XCTAssertEqual(started, draft)
            await store.send(presentation)
            await store.send(.submit)
            XCTAssertEqual(store.state.pending, draft)
            XCTAssertTrue(store.state.inFlight)
            XCTAssertEqual(store.state.text, "Keep the written reply")
            await gate.release(canonical)
            await store.receive(\.prepared) {
                $0.pending = nil; $0.inFlight = false; $0.stamp = "Sent"
            }
            await store.receive(\.sent, canonical)
            await store.finish()
            let preparations = await gate.requests
            XCTAssertEqual(preparations, [draft], "replacement taps cannot allocate another intent before the first preparation finishes")
        }
    }

    func testAStampedDeliveryStillAllowsTheExistingComposerReset() async throws {
        let action = try XCTUnwrap(try state().action)
        let presentations = [ActionFeature.Action.present(action),
            .presentForm(action, ["text": .string("The next draft")], requiresAttachments: true)]
        for (index, presentation) in presentations.enumerated() {
            var initial = try state()
            initial.pending = try canonical(draftID)
            initial.inFlight = true; initial.stamp = "Sent"
            let store = TestStore(initialState: initial) { ActionFeature() }
            await store.send(presentation) {
                $0.pending = nil; $0.inFlight = false; $0.stamp = nil; $0.text = ""
                if index == 1 {
                    $0.fields = ["text": .string("The next draft")]
                    $0.requiresAttachments = true
                }
            }
            XCTAssertNil(store.state.pending)
            XCTAssertFalse(store.state.inFlight)
            await store.finish()
        }
    }

    func testDismissAllowsANewPresentationWhileItsOldPreparationCannotHandOff() async throws {
        let gate = ActionPreparationGate()
        var starts = gate.starts.makeAsyncIterator()
        let draft = request(draftID)
        let canonical = try canonical(draftID)
        let initial = try state(handsOff: true)
        let action = try XCTUnwrap(initial.action)
        let store = TestStore(initialState: initial) { ActionFeature() } withDependencies: {
            $0.uuid = .constant(UUID(uuidString: draftID)!)
            $0.viewsClient.prepare = { try await gate.prepare($0) }
            $0.viewsClient.perform = { _ in XCTFail("the dismissed preparation cannot dispatch"); throw CancellationError() }
        }
        await store.send(.submit) { $0.pending = draft; $0.inFlight = true; $0.sends = 1 }
        let started = await starts.next()
        XCTAssertEqual(started, draft)
        await store.send(.dismiss) { $0.action = nil; $0.pending = nil; $0.inFlight = false }
        await store.send(.present(action)) { $0.action = action; $0.text = "" }
        await gate.release(canonical)
        await store.receive(\.prepared)
        XCTAssertEqual(store.state.action, action)
        XCTAssertNil(store.state.pending)
        XCTAssertNil(store.state.stamp)
        XCTAssertFalse(store.state.inFlight)
        await store.finish()
    }

}
