import AlondraContract
import AlondraDesign
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import Foundation
import XCTest

/// The detail of a card that asks several questions: the same stepper as the card, with the
/// answers handed off together when the last one is given.
final class CardDetailQuestionsTests: XCTestCase {
    @MainActor
    private func store(feed: FeedStub = FeedStub(), dismissed: LockIsolated<Int> = LockIsolated(0)) throws
        -> TestStoreOf<CardDetailFeature> {
        let document = try Fixtures.questionsCard()
        var state = CardDetailFeature.State(key: ViewKey(rawValue: document.key))
        state.screen.document = document
        state.screen.phase = .loaded
        state.actions.dataEpoch = document.dataEpoch
        let store = TestStore(initialState: state) {
            CombineReducers {
                CardDetailFeature()
                Reduce { _, action in
                    if case .delegate(.handOff(let request)) = action { feed.deliver(request) }
                    return .none
                }
            }
        } withDependencies: {
            $0.viewsClient.perform = feed.perform()
            $0.dismiss = DismissEffect { dismissed.withValue { $0 += 1 } }
        }
        store.exhaustivity = .off
        return store
    }

    @MainActor
    func testTheBarShowsTheButtonsOfTheQuestionShownAndTheHeaderItsWords() throws {
        let store = try store()
        XCTAssertEqual(store.state.step?.need, "How do the devices reach the Mac?")
        XCTAssertEqual(store.state.buttons.map(\.label), ["★ LAN and Tailscale", "LAN only"])
        XCTAssertEqual(store.state.menu.map(\.label), ["Write my own answer"])
        XCTAssertEqual(store.state.stepIndex, 0)
    }

    @MainActor
    func testAnsweringEachQuestionAdvancesAndTheLastHandsOffAllTheAnswersAndClosesTheDetail() async throws {
        let feed = FeedStub()
        let dismissed = LockIsolated(0)
        let store = try store(feed: feed, dismissed: dismissed)

        await store.send(.actionTapped(store.state.buttons[0]))
        XCTAssertEqual(store.state.step?.need, "Which port should it listen on?")
        XCTAssertEqual(store.state.step?.why, "The old one is taken by another service.")
        XCTAssertEqual(feed.requests, [])
        XCTAssertEqual(dismissed.value, 0)

        await store.send(.actionTapped(store.state.buttons[1]))
        await store.send(.actionTapped(store.state.buttons[1]))
        await store.skipReceivedActions()

        let request = try XCTUnwrap(feed.requests.first, "the last answer is handed off")
        XCTAssertEqual(feed.requests.count, 1)
        XCTAssertEqual(request.actionId, "ans:\(Fixtures.questionsID)@1")
        XCTAssertNil(request.optionId)
        XCTAssertEqual(request.input.fields?["answers"], JSONValue.array([
            .object(["option_id": .string("o1"), "text": .null]),
            .object(["option_id": .string("o0"), "text": .null]),
            .object(["option_id": .string("o1"), "text": .null]),
        ]))
        XCTAssertEqual(dismissed.value, 1)
    }

    @MainActor
    func testAnAnswerInYourOwnWordsAndGoingBackWorkInTheDetailToo() async throws {
        let feed = FeedStub()
        let store = try store(feed: feed)

        await store.send(.actionTapped(store.state.buttons[1]))
        await store.send(.previousQuestion)
        XCTAssertEqual(store.state.stepIndex, 0)
        await store.send(.actionTapped(try XCTUnwrap(store.state.menu.first)))
        XCTAssertNotNil(store.state.composition)
        await store.send(.noteChanged("Both, please"))
        await store.send(.compositionConfirmed)
        await store.send(.actionTapped(store.state.buttons[0]))
        await store.send(.actionTapped(store.state.buttons[0]))
        await store.skipReceivedActions()

        XCTAssertEqual(feed.requests.first?.input.fields?["answers"], JSONValue.array([
            .object(["option_id": .null, "text": .string("Both, please")]),
            .object(["option_id": .string("o1"), "text": .null]),
            .object(["option_id": .string("o0"), "text": .null]),
        ]))
    }
}
