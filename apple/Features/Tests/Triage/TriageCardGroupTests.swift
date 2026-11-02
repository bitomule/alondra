import AlondraContract
import AlondraDesign
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import Foundation
import XCTest

/// One of several questions asked together is an ordinary card; the only thing that sets it apart
/// is where it stands among the others.
final class TriageCardGroupTests: XCTestCase {
    private let id = "g-q2"

    @MainActor
    private func store(feed: FeedStub = FeedStub()) throws -> TestStoreOf<TriageCardFeature> {
        let document = try Fixtures.document("card-group")
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
    func testAQuestionOfAGroupSaysWhereItStandsAndIsAnsweredLikeAnyOtherCard() async throws {
        let feed = FeedStub()
        let store = try store(feed: feed)
        XCTAssertEqual(store.state.card?.meta, "Build a feature · 2 of 3")
        XCTAssertEqual(store.state.card?.eyebrow?.text, "Decide · 2 of 3")
        XCTAssertEqual(store.state.buttons.map(\.optionId), ["o0", "o1"])

        await store.send(.actionTapped(store.state.buttons[1]))
        await store.skipReceivedActions()
        let request = try XCTUnwrap(feed.requests.first)
        XCTAssertEqual(feed.requests.count, 1, "an answer is sent at once, one per card")
        XCTAssertEqual(request.actionId, "ans:\(id)@1")
        XCTAssertEqual(request.optionId, "o1")
        XCTAssertNil(request.input.fields, "no combined answers: each card is answered on its own")
    }
}
