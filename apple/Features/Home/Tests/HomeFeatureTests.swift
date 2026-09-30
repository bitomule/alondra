import AlondraHomeFeature
import AlondraViews
import ComposableArchitecture
import Foundation
import XCTest

final class HomeFeatureTests: XCTestCase {
    @MainActor
    func testLoadFailureKeepsCachedHomeForTransientConnectionLoss() async throws {
        let fixtureRoot = URL(fileURLWithPath: try XCTUnwrap(ProcessInfo.processInfo.environment["TEST_SRCDIR"]))
            .appendingPathComponent(ProcessInfo.processInfo.environment["TEST_WORKSPACE"] ?? "_main")
            .appendingPathComponent("fixtures/views")
        let data = try Data(contentsOf: fixtureRoot.appendingPathComponent("home-ios.json"))
        let document = try ViewsCodec.decode(ViewDocument.self, from: data)
        var state = HomeFeature.State()
        state.document = document
        state.status = document.status
        state.loading = true
        state.error = nil

        let store = TestStore(initialState: state) { HomeFeature() }
        await store.send(.loadFailed("offline")) {
            $0.loading = false
            $0.error = "offline"
        }
        XCTAssertEqual(store.state.document, document)
    }

    @MainActor
    func testHomeKeepsServerOrderAndTheTwoInlineCards() async throws {
        let fixtureRoot = URL(fileURLWithPath: try XCTUnwrap(ProcessInfo.processInfo.environment["TEST_SRCDIR"]))
            .appendingPathComponent(ProcessInfo.processInfo.environment["TEST_WORKSPACE"] ?? "_main")
            .appendingPathComponent("fixtures/views")
        let data = try Data(contentsOf: fixtureRoot.appendingPathComponent("home-ios.json"))
        let document = try ViewsCodec.decode(ViewDocument.self, from: data)
        let ids = try XCTUnwrap(document.sections.first(where: { $0.id == "queue" })?.cardIds)
        XCTAssertEqual(ids.count, 4)

        let store = TestStore(initialState: HomeFeature.State()) { HomeFeature() }
        await store.send(.loaded(.document(document))) {
            $0.document = document
            $0.status = document.status
            $0.orderedIDs = ids
            $0.cards = Dictionary(uniqueKeysWithValues: document.sections[0].items.compactMap { item -> (String, Card)? in
                guard case let .card(card) = item else { return nil }
                return (card.id, card)
            })
            $0.actions.dataEpoch = document.dataEpoch
        }
        XCTAssertEqual(store.state.currentID, ids[0])
        XCTAssertNotNil(store.state.currentCard)
        XCTAssertEqual(store.state.cards.count, 2)
        await store.send(.stale(["unrelated-view"]))
    }
}
