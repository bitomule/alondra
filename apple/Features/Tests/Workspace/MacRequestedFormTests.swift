import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import XCTest

final class MacRequestedFormTests: XCTestCase {
    @MainActor
    func testTheMacSendsTheFilledFormWithTheOptionThatAcceptsIt() async throws {
        let feed = FeedStub()
        let document = try Fixtures.document("card-elicitation-form-ios")
        let card = try XCTUnwrap(document.sections.flatMap(\.items).compactMap { item -> Card? in if case let .card(card) = item { card } else { nil } }.first)
        let form = try XCTUnwrap(card.form)
        let accept = try XCTUnwrap(card.actions.first { form.isFilled(by: $0) })
        var state = MacPresentationFeature.State()
        state.detailDocument = document
        state.selection = .viewKey(document.key); state.selectedViewKey = document.key
        let store = TestStore(initialState: state) { MacPresentationFeature() } withDependencies: {
            $0.continuousClock = TestClock()
            $0.viewsClient.perform = feed.perform()
        }
        store.exhaustivity = .off

        await store.send(.sendForm(accept, form, content: ["remember": .bool(true)]))
        await store.skipReceivedActions()

        XCTAssertEqual(feed.requests.map(\.optionId), ["accept"])
        XCTAssertEqual(feed.requests.first?.input.content, ["remember": .bool(true)])
        XCTAssertFalse(store.state.actionPresented, "the card's own form is the answer: no sheet opens")
    }
}
