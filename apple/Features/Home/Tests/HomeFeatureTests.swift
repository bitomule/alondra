import AlondraHomeFeature
import AlondraViews
import AlondraViewsDependency
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
    func testHomeKeepsServerOrderAndPrefetchesTheFirstTwoCards() async throws {
        let fixtureRoot = URL(fileURLWithPath: try XCTUnwrap(ProcessInfo.processInfo.environment["TEST_SRCDIR"]))
            .appendingPathComponent(ProcessInfo.processInfo.environment["TEST_WORKSPACE"] ?? "_main")
            .appendingPathComponent("fixtures/views")
        let data = try Data(contentsOf: fixtureRoot.appendingPathComponent("home-ios.json"))
        let document = try ViewsCodec.decode(ViewDocument.self, from: data)
        let ids = try XCTUnwrap(document.sections.first(where: { $0.id == "queue" })?.cardIds)
        XCTAssertEqual(ids.count, 4)
        XCTAssertTrue(document.sections[0].items.isEmpty)

        let store = TestStore(initialState: HomeFeature.State()) { HomeFeature() } withDependencies: {
            $0.viewsClient.load = { _, _ in
                try await Task.sleep(for: .seconds(60))
                throw CancellationError()
            }
        }
        await store.send(.loaded(.document(document))) {
            $0.document = document
            $0.status = document.status
            $0.orderedIDs = ids
            $0.loadingCardIDs = Set(ids.prefix(2))
            $0.actions.dataEpoch = document.dataEpoch
        }
        XCTAssertEqual(store.state.currentID, ids[0])
        XCTAssertNil(store.state.currentCard)
        XCTAssertEqual(store.state.loadingCardIDs, Set(ids.prefix(2)))
        await store.send(.stale(["unrelated-view"]))
        await store.send(.disappeared) { $0.loadingCardIDs = [] }
    }
}
