import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import Foundation
import XCTest

final class DurableActionEpochTests: XCTestCase {
    private let oldID = "00000000-0000-4000-8000-000000000001"
    private let newID = "00000000-0000-4000-8000-000000000002"
    private func request(_ id: String, epoch: String) -> ActionRequest {
        ActionRequest(commandId: id, dataEpoch: epoch, client: .ios, lang: "en",
            actionId: "reply:q-1@1", input: ActionInput(text: "Answer"), resolvesCard: false)
    }

    @MainActor
    func testARecreatedDraftCannotReuseAnUncertainCommandFromAnotherEpoch() async throws {
        var state = ActionFeature.State(dataEpoch: "new-epoch", client: .ios, lang: "en")
        state.action = try ContractCodec.decode(AlondraContract.Action.self, from: Data(#"{"id":"reply:q-1@1","label":"Reply","style":"primary","presentation":"button","input":{"kind":"text","required":true,"multiline":true},"attachments":false,"enabled":true,"resolves_card":false}"#.utf8))
        state.text = "Answer"
        state.unsure = request(oldID, epoch: "old-epoch")
        state.handsOff = true
        let store = TestStore(initialState: state) { ActionFeature() } withDependencies: {
            $0.uuid = .constant(UUID(uuidString: self.newID)!)
        }
        let current = request(newID, epoch: "new-epoch")
        await store.send(.submit) {
            $0.stamp = "Sent"
            $0.sends = 1
        }
        await store.receive(\.sending, current)
        await store.receive(\.sent, current)
        await store.finish()
    }

    @MainActor
    func testLateOldEpochRepliesCannotSettleOrReportAnErrorOnTheCurrentDraft() async throws {
        let old = request(oldID, epoch: "old-epoch")
        let reply = try ContractCodec.decode(ActionResponse.self, from:
            Data(#"{"protocol":1,"outcome":"applied","stamp":"Saved old answer","revision":2}"#.utf8))
        for action in [ActionFeature.Action.answered(old, reply), .undelivered(old, .unsure)] {
            var state = ActionFeature.State(dataEpoch: "new-epoch", client: .ios, lang: "en")
            state.pending = request(newID, epoch: "new-epoch")
            state.inFlight = true
            state.stamp = "Sending current answer"
            let store = TestStore(initialState: state) { ActionFeature() }
            await store.send(action)
            await store.finish()
        }
    }

    private static func macDocument(key: String, epoch: String) throws -> ViewDocument {
        let rows = ["a", "b"].map {
            #"{"type":"row","id":"\#($0)","icon":"question","title":"Card \#($0)","depth":0,"struck":false,"link":{"kind":"view","key":"card:\#($0)"},"children":[]}"#
        }.joined(separator: ",")
        let sections = key == "sidebar" ? #"[{"id":"waiting","collapsed":false,"items":[\#(rows)]}]"# : "[]"
        return try ContractCodec.decode(ViewDocument.self, from: Data(#"{"protocol":1,"view":"\#(key)","key":"\#(key)","client":"mac","version":"\#(epoch)","cursor":"cursor","data_epoch":"\#(epoch)","status":{"version":"status","summary":"Ready","badge":0,"counts":[],"gauges":[],"orchestrator":{"text":"Ready","tone":"success"}},"sections":\#(sections),"notices":[]}"#.utf8))
    }

    @MainActor
    func testANewSidebarFencesOldActionCallbacksBeforeTheDetailArrives() async throws {
        let oldDetail = try Self.macDocument(key: "card:a", epoch: "old-epoch")
        let newSidebar = try Self.macDocument(key: "sidebar", epoch: "new-epoch")
        let newDetail = try Self.macDocument(key: "card:a", epoch: "new-epoch")
        let old = ActionRequest(commandId: oldID, dataEpoch: "old-epoch", client: .mac, lang: "en",
            actionId: "ans:a@1", input: ActionInput(text: "Answer"), resolvesCard: true)
        let reply = try ContractCodec.decode(ActionResponse.self, from:
            Data(#"{"protocol":1,"outcome":"applied","stamp":"Saved old answer","revision":2}"#.utf8))
        var state = MacPresentationFeature.State()
        state.sidebarDocument = try Self.macDocument(key: "sidebar", epoch: "old-epoch")
        state.detailDocument = oldDetail
        state.selection = .viewKey("card:a"); state.selectedViewKey = "card:a"
        state.actionFeature.dataEpoch = "old-epoch"
        state.actionFeature.pending = old; state.actionFeature.inFlight = true
        state.actionFeature.stamp = "Sending old answer"
        state.actionTarget = "a"; state.actionPresented = true
        let store = TestStore(initialState: state) { MacPresentationFeature() } withDependencies: {
            $0.continuousClock = TestClock()
            $0.viewsClient.openScreen = { _, _ in }
            $0.viewsClient.closeScreen = { _ in }
            $0.viewsClient.load = { key, _ in .document(try Self.macDocument(key: key.rawValue, epoch: "new-epoch")) }
        }
        await store.send(.viewLoaded("sidebar", .document(newSidebar))) {
            $0.sidebarDocument = newSidebar; $0.detailDocument = nil
            $0.actionFeature.dataEpoch = "new-epoch"
            $0.actionTarget = nil; $0.actionPresented = false
            $0.optimistic.reconcile(newSidebar.version, dataEpoch: newSidebar.dataEpoch)
        }
        await store.send(.action(.sent(old)))
        await store.send(.action(.settled(old, applied: true)))
        await store.send(.action(.answered(old, reply)))
        await store.send(.action(.undelivered(old, .unsure)))
        await store.send(.viewLoaded("card:a", .document(oldDetail)))
        XCTAssertEqual(store.state.selection, .viewKey("card:a"))
        XCTAssertEqual(store.state.selectedViewKey, "card:a")
        XCTAssertFalse(store.state.optimistic.hides("a"))
        XCTAssertTrue(store.state.unsure.isEmpty)
        await store.send(.viewLoaded("card:a", .document(newDetail))) { $0.detailDocument = newDetail }
        await store.finish()
    }

    @MainActor
    func testOldFailuresCannotRestoreTextIntoNewEpochComposers() async {
        let old = request(oldID, epoch: "old-epoch")
        var task = TaskDetailFeature.State(key: .task("fixture-task"))
        task.composer.dataEpoch = "new-epoch"
        let taskStore = TestStore(initialState: task) { TaskDetailFeature() }
        await taskStore.send(.composer(.settled(old, applied: false)))
        await taskStore.finish()
    }

    func testChangingTheEpochReleasesTheOldDraftsDeliveryState() {
        var state = ActionFeature.State(dataEpoch: "old-epoch", client: .ios, lang: "en")
        let old = request(oldID, epoch: "old-epoch")
        state.pending = old; state.unsure = old; state.inFlight = true
        state.stamp = "Waiting for old answer"; state.error = "Old connection failed"
        state.dataEpoch = "new-epoch"
        XCTAssertNil(state.pending)
        XCTAssertNil(state.unsure)
        XCTAssertFalse(state.inFlight, "the new card must not keep its send button disabled by the old delivery")
        XCTAssertNil(state.stamp)
        XCTAssertNil(state.error)
    }

    func testAnOldPendingOutcomeCannotHideTheSameTargetInANewEpoch() {
        let old = request(oldID, epoch: "old-epoch")
        var optimistic = Optimistic()
        optimistic.reconcile("old-document", dataEpoch: "old-epoch")
        optimistic.begin("same-card", old)
        XCTAssertTrue(optimistic.hides("same-card"))
        optimistic.reconcile("new-document", dataEpoch: "new-epoch")
        XCTAssertFalse(optimistic.hides("same-card"))
        optimistic.settle(old, applied: true, shown: "new-document")
        XCTAssertFalse(optimistic.hides("same-card"))
        optimistic.begin("same-card", old, dataEpoch: "new-epoch")
        XCTAssertFalse(optimistic.hides("same-card"), "a late sent event cannot hide the new epoch's card")
        optimistic.begin("same-card", request(newID, epoch: "new-epoch"))
        XCTAssertTrue(optimistic.hides("same-card"))
        optimistic.settle(request(newID, epoch: "new-epoch"), applied: false, shown: "new-document")
        XCTAssertFalse(optimistic.hides("same-card"))
    }
}
