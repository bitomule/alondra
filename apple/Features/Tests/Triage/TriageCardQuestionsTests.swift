import AlondraContract
import AlondraDesign
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import Foundation
import XCTest

/// A card that asks several questions: one at a time, nothing sent until the last is answered.
final class TriageCardQuestionsTests: XCTestCase {
    private let id = Fixtures.questionsID

    @MainActor
    private func store(feed: FeedStub = FeedStub()) throws -> TestStoreOf<TriageCardFeature> {
        let document = try Fixtures.questionsCard()
        var state = TriageCardFeature.State(id: id)
        state.screen.document = document
        state.screen.phase = .loaded
        state.actions.dataEpoch = document.dataEpoch
        let store = TestStore(initialState: state) { TriageCardFeature() } withDependencies: {
            $0.viewsClient.perform = feed.perform()
        }
        store.exhaustivity = .off
        return store
    }

    @MainActor
    func testTheCardShowsTheFirstQuestionWithItsOwnButtonsAndTheWayToWriteUnderMore() throws {
        let store = try store()
        XCTAssertEqual(store.state.step?.need, "How do the devices reach the Mac?")
        XCTAssertEqual(store.state.stepIndex, 0)
        XCTAssertEqual(store.state.buttons.map(\.label), ["★ LAN and Tailscale", "LAN only"])
        XCTAssertEqual(store.state.buttons.map(\.optionId), ["o1", "o0"])
        XCTAssertEqual(store.state.menu.map(\.label), ["Write my own answer"])
        XCTAssertEqual(store.state.card?.steps.count, 3)
    }

    @MainActor
    func testTheWhyEachOptionRowCountsOnlyTheOptionsOfTheQuestionShown() async throws {
        let store = try store()
        XCTAssertNil(store.state.card?.contentRow, "the card's own row would count every question's options")
        XCTAssertEqual(store.state.step?.contentRow?.summary, "2 options")
        await store.send(.actionTapped(store.state.buttons[0]))
        XCTAssertEqual(store.state.step?.contentRow?.summary, "2 options")
        await store.send(.actionTapped(store.state.buttons[0]))
        XCTAssertEqual(store.state.step?.contentRow?.summary, "3 options", "the third question has three")
    }

    @MainActor
    func testEachAnswerAdvancesToTheNextQuestionAndSendsNothingUntilTheLastOne() async throws {
        let feed = FeedStub()
        let store = try store(feed: feed)

        await store.send(.actionTapped(store.state.buttons[0]))
        XCTAssertEqual(store.state.step?.need, "Which port should it listen on?")
        XCTAssertEqual(store.state.stepIndex, 1)
        XCTAssertEqual(store.state.buttons.map(\.label), ["★ 8080", "7777"], "the second question has its own options")
        XCTAssertEqual(feed.requests, [], "one answer is not yet the answer")
        XCTAssertNil(store.state.stamp)

        await store.send(.actionTapped(store.state.buttons[1]))
        XCTAssertEqual(store.state.stepIndex, 2)
        XCTAssertEqual(feed.requests, [])
    }

    @MainActor
    func testTheLastAnswerSendsEveryChoiceTogetherInOneRequestAndTheStampShowsAtOnce() async throws {
        let feed = FeedStub()
        let store = try store(feed: feed)

        await store.send(.actionTapped(store.state.buttons[0]))
        await store.send(.actionTapped(store.state.buttons[1]))
        await store.send(.actionTapped(store.state.buttons[0]))
        await store.skipReceivedActions()

        let request = try XCTUnwrap(feed.requests.first, "the last answer sends")
        XCTAssertEqual(feed.requests.count, 1)
        XCTAssertEqual(request.actionId, "ans:\(id)@1")
        XCTAssertNil(request.optionId, "the choices travel in the fields, not as one option")
        XCTAssertEqual(request.input.fields?["answers"], JSONValue.array([
            .object(["option_id": .string("o1"), "text": .null]),
            .object(["option_id": .string("o0"), "text": .null]),
            .object(["option_id": .string("o0"), "text": .null]),
        ]))
        XCTAssertEqual(store.state.stamp, "Answered")
    }

    @MainActor
    func testWritingYourOwnAnswerAsksForTheWordsAndAnswersTheQuestionShown() async throws {
        let feed = FeedStub()
        let store = try store(feed: feed)
        let write = try XCTUnwrap(store.state.menu.first)

        await store.send(.actionTapped(store.state.buttons[0]))
        await store.send(.actionTapped(write))
        XCTAssertNotNil(store.state.composition, "the words come first")
        XCTAssertEqual(store.state.stepIndex, 1)
        await store.send(.noteChanged("Use 9090"))
        await store.send(.compositionConfirmed)
        XCTAssertEqual(store.state.stepIndex, 2)
        XCTAssertEqual(feed.requests, [])

        await store.send(.actionTapped(store.state.buttons[1]))
        await store.skipReceivedActions()
        XCTAssertEqual(feed.requests.first?.input.fields?["answers"], JSONValue.array([
            .object(["option_id": .string("o1"), "text": .null]),
            .object(["option_id": .null, "text": .string("Use 9090")]),
            .object(["option_id": .string("o1"), "text": .null]),
        ]))
    }

    @MainActor
    func testGoingBackReopensTheQuestionBeforeAndItsAnswerCanChange() async throws {
        let feed = FeedStub()
        let store = try store(feed: feed)

        await store.send(.actionTapped(store.state.buttons[0]))
        await store.send(.previousQuestion)
        XCTAssertEqual(store.state.stepIndex, 0)
        XCTAssertFalse(store.state.questions.canGoBack)
        await store.send(.actionTapped(store.state.buttons[1]))
        await store.send(.actionTapped(store.state.buttons[0]))
        await store.send(.actionTapped(store.state.buttons[0]))
        await store.skipReceivedActions()

        XCTAssertEqual(feed.requests.first?.input.fields?["answers"]?.firstOptionID, "o0", "the changed answer is the one sent")
    }

    @MainActor
    func testAnAnswerTheDaemonDoesNotApplyStartsTheQuestionsOver() async throws {
        let feed = FeedStub()
        let document = try Fixtures.questionsCard()
        var state = TriageCardFeature.State(id: id)
        state.screen.document = document
        state.screen.phase = .loaded
        state.actions.dataEpoch = document.dataEpoch
        let store = TestStore(initialState: state) { TriageCardFeature() } withDependencies: {
            $0.viewsClient.perform = feed.perform(#"{"protocol":1,"outcome":"no_longer_available","message":"This item changed"}"#)
        }
        store.exhaustivity = .off

        for _ in 0..<3 { await store.send(.actionTapped(store.state.buttons[0])) }
        await store.skipReceivedActions()
        XCTAssertEqual(feed.requests.count, 1)
        XCTAssertEqual(store.state.stepIndex, 0, "the card asks again from the first question")
        XCTAssertFalse(store.state.questions.canGoBack)
    }
}

private extension JSONValue {
    var firstOptionID: String? {
        guard case .array(let answers) = self, case .object(let first)? = answers.first,
              case .string(let id)? = first["option_id"] else { return nil }
        return id
    }
}
