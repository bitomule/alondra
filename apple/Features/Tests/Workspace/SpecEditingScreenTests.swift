import AlondraContract
import AlondraFeed
import AlondraFeatures
import ComposableArchitecture
import Foundation
import XCTest

/// Editing the specs of a plan from the phone: the edit shows at once, a Mac that does not answer is
/// said so after 30 seconds, and one the Mac refuses goes back with its reason beside the field.
@MainActor
final class SpecEditingScreenTests: XCTestCase {
    private let form = try! ContractCodec.decode(AlondraContract.Form.self, from: Data(#"{"id":"task_specs-scenario","fields":[{"id":"title","label":"Title","kind":{"kind":"text"},"value":"Part","required":true,"reload_on_change":false},{"id":"steps","label":"Steps","kind":{"kind":"steps"},"value":"Given a","required":true,"reload_on_change":false}],"submit":{"id":"spec_edit:t-1@7~sc-c1","label":"Save the scenario","style":"primary","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":true}}"#.utf8))

    private func sent() -> ActionRequest {
        ActionRequest(commandId: "c1", dataEpoch: "e", client: .ios, lang: "en", actionId: "spec_edit:t-1@7~sc-c1", optionId: nil,
                      input: ActionInput(fields: ["title": .string("Changed"), "steps": .string("Given a\nSomething odd")]), resolvesCard: nil)
    }

    private func screen(clock: TestClock<Duration> = TestClock()) -> TestStoreOf<ViewsScreenFeature> {
        var state = ViewsScreenFeature.State(key: ViewKey(rawValue: "task_specs:t-1"), owner: "specs-test")
        state.$specEdits.withLock { $0 = SpecEdits() }
        state.form = form
        state.action.dataEpoch = "e"
        let store = TestStore(initialState: state) { ViewsScreenFeature() } withDependencies: { $0.continuousClock = clock }
        store.exhaustivity = .off
        return store
    }

    func testAnEditShowsAtOnceAsSavingAndTheFormClosesOnceItIsSaved() async {
        let store = screen()
        await store.send(.action(.sending(sent())))
        XCTAssertEqual(store.state.specEdits.saving, ["task_specs:t-1:scenario:c1": .saving])
        await store.send(.action(.sent(sent())))
        await store.receive(\.action.dismiss)
        XCTAssertNil(store.state.action.action)
    }

    func testAMacThatDoesNotConfirmInThirtySecondsIsSaidSoWhileItKeepsRetrying() async {
        let clock = TestClock()
        let store = screen(clock: clock)
        await store.send(.action(.sending(sent())))
        await clock.advance(by: .seconds(29))
        XCTAssertEqual(store.state.specEdits.saving, ["task_specs:t-1:scenario:c1": .saving])
        await clock.advance(by: .seconds(1))
        await store.receive(\.specEditOverdue)
        XCTAssertEqual(store.state.specEdits.saving, ["task_specs:t-1:scenario:c1": .overdue])
    }

    func testARefusedEditGoesBackAndItsFormOpensWithTheReasonBesideTheField() async throws {
        let store = screen()
        let request = sent()
        await store.send(.action(.sending(request)))
        await store.send(.action(.settled(request, applied: false)))
        let refusal = try ContractCodec.decode(ActionResponse.self, from: Data(#"{"protocol":1,"outcome":"rejected","message":"Line 2: a step starts with Given, When, Then, And or But","field":"steps"}"#.utf8))
        await store.send(.action(.answered(request, refusal)))
        await store.receive(\.presentForm)
        await store.receive(\.action.fieldsChanged)
        await store.receive(\.action.fieldRejected)
        XCTAssertTrue(store.state.specEdits.saving.isEmpty, "it is back to what it was")
        XCTAssertEqual(store.state.action.fieldError, ActionFeature.FieldError(field: "steps", message: "Line 2: a step starts with Given, When, Then, And or But"))
        XCTAssertEqual(store.state.action.fields["steps"], .string("Given a\nSomething odd"), "what the person wrote is still there")
        XCTAssertEqual(store.state.form, form)
    }
}
