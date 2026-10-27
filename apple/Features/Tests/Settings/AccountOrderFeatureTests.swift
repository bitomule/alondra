import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import Foundation
import XCTest

@MainActor
final class AccountOrderFeatureTests: XCTestCase {
    private func document(version: String = "cfg-a", epoch: String = "epoch") throws -> ViewDocument {
        let account: (String) -> [String: Any] = { provider in [
            "key": ["provider": provider, "account": "work"], "position": provider == "codex" ? 1 : 2,
            "availability": ["text": "Signed in", "tone": "success"], "active_nodes": 0,
            "freshness": ["text": "Usage not reported", "tone": "neutral"], "windows": []
        ] }
        let rows: [[String: Any]] = ["codex", "claude"].map { provider in ["type": "row", "id": provider + "-work", "icon": "machine", "title": "Work", "depth": 0, "struck": false, "account": account(provider)] }
        let action: [String: Any] = ["id": "account_order:accounts@1~" + version, "label": "Reorder accounts", "style": "secondary", "presentation": "button", "input": ["kind": "none"], "attachments": false, "enabled": true]
        let view: [String: Any] = ["protocol": 1, "view": "accounts", "key": "accounts", "client": "ios", "version": "view-" + version, "cursor": "cursor", "data_epoch": epoch,
            "account_configuration": ["version": version], "status": ["version": "s", "summary": "Ready", "badge": 0, "counts": [], "gauges": [], "orchestrator": ["text": "idle", "tone": "neutral"]],
            "sections": [["id": "accounts", "title": "Accounts", "collapsed": false, "items": rows, "actions": [action]]]]
        return try ContractCodec.decode(ViewDocument.self, from: JSONSerialization.data(withJSONObject: view))
    }

    private func state() throws -> ServerSettingsFeature.State {
        let view = try document()
        var state = ServerSettingsFeature.State(key: .accounts, client: .ios, lang: "en")
        state.document = view; state.actions.dataEpoch = view.dataEpoch
        return state
    }

    func testReorderStaysADraftAndCancelDoesNotChangeTheCommittedOrder() async throws {
        let initial = try state()
        let action = try XCTUnwrap(initial.reorderAction)
        let store = TestStore(initialState: initial) { ServerSettingsFeature() }
        await store.send(.orderBegan) { $0.orderDraft = AccountOrderDraft(rows: initial.accountRows, configurationVersion: "cfg-a", dataEpoch: "epoch", action: action) }
        await store.send(.orderMoved("codex-work", to: 1)) { $0.orderDraft?.rows.reverse() }
        XCTAssertEqual(store.state.accountRows.compactMap(\.account).map(\.key.provider), [.codex, .claude])
        await store.send(.orderDraftPresented(nil)) { $0.orderDraft = nil }
        XCTAssertEqual(store.state.accountRows.map(\.id), ["codex-work", "claude-work"])
    }

    func testAnotherConfigRevisionBlocksSaveAndRetainsTheDraftUntilReload() async throws {
        var initial = try state()
        initial.orderDraft = AccountOrderDraft(rows: Array(initial.accountRows.reversed()), configurationVersion: "cfg-a", dataEpoch: "epoch", action: try XCTUnwrap(initial.reorderAction))
        let next = try document(version: "cfg-b")
        let store = TestStore(initialState: initial) { ServerSettingsFeature() }
        await store.send(.loaded(next)) { $0.document = next; $0.orderDraft?.conflict = true }
        await store.send(.orderSaved)
        XCTAssertEqual(store.state.orderDraft?.rows.map(\.id), ["claude-work", "codex-work"])
    }

    func testAnUncertainSaveRetainsItsCommandAndCannotBeResentOrDismissedUntilItsOwnAnswerArrives() async throws {
        var initial = try state()
        let action = try XCTUnwrap(initial.reorderAction)
        initial.orderDraft = AccountOrderDraft(rows: Array(initial.accountRows.reversed()), configurationVersion: "cfg-a", dataEpoch: "epoch", action: action)
        let request = ActionRequest(commandId: "save-order", dataEpoch: "epoch", client: .ios, lang: "en", actionId: try XCTUnwrap(action.id))
        initial.orderDraft?.saving = true; initial.orderDraft?.commandNonce = ActionRequest.deliveryNonce(request.commandId)
        initial.actions.action = action; initial.actions.pending = request; initial.actions.inFlight = true
        let store = TestStore(initialState: initial) { ServerSettingsFeature() }
        await store.send(.actions(.undelivered(request, .unsure))) {
            $0.actions.pending = nil; $0.actions.inFlight = false; $0.actions.unsure = request
            $0.actions.error = SendFailure.unsure.message; $0.error = SendFailure.unsure.message
        }
        await store.receive(\.actions.delegate.failed)
        await store.send(.orderSaved)
        await store.send(.orderDraftPresented(nil))
        XCTAssertTrue(store.state.orderDraft?.saving == true)
        let other = ActionRequest(commandId: "another-save", dataEpoch: "epoch", client: .ios, lang: "en", actionId: try XCTUnwrap(action.id))
        let answer = try Responses.applied("Order saved")
        await store.send(.actions(.answered(other, answer)))
        await store.receive(\.actions.delegate.completed) { $0.notice = "Order saved" }
        XCTAssertNotNil(store.state.orderDraft, "another command's answer cannot retire this pending save")
        await store.send(.actions(.answered(request, answer))) { $0.orderDraft = nil }
        await store.receive(\.actions.delegate.completed) { $0.error = nil }
    }
}
