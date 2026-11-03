import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import XCTest

@MainActor
final class MetricsFeatureTests: XCTestCase {
    private static let week = "metrics?period=7d&compare=previous"
    private static let month = "metrics?period=30d&compare=previous"

    private func metrics(key: String = week) throws -> ViewDocument {
        let environment = ProcessInfo.processInfo.environment
        let url = URL(fileURLWithPath: try XCTUnwrap(environment["TEST_SRCDIR"]))
            .appendingPathComponent(environment["TEST_WORKSPACE"] ?? "_main")
            .appendingPathComponent("fixtures/views/metrics/metrics-ios.json")
        var object = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any])
        object["key"] = key
        return try ContractCodec.decode(ViewDocument.self, from: JSONSerialization.data(withJSONObject: object))
    }

    private func section(_ document: ViewDocument, _ id: String) throws -> ViewSection {
        try XCTUnwrap(document.sections.first { $0.id == id })
    }

    private func row(_ section: ViewSection, _ id: String) throws -> AlondraContract.Row {
        try XCTUnwrap(section.items.compactMap { item -> AlondraContract.Row? in if case .row(let row) = item { row } else { nil } }.first { $0.id == id })
    }

    func testADaemonWithoutTheScreenLeavesSettingsWithoutIt() async {
        let feed = FeedStub()
        let store = TestStore(initialState: MetricsFeature.State()) { MetricsFeature() } withDependencies: {
            $0.viewsClient.observe = feed.observe
        }
        await store.send(.task)
        await store.receive(.missing)
        XCTAssertFalse(store.state.offered)
        XCTAssertNil(store.state.document)
    }

    func testADaemonWithTheScreenOffersIt() async throws {
        let document = try metrics(key: "metrics")
        let feed = FeedStub([document])
        let store = TestStore(initialState: MetricsFeature.State()) { MetricsFeature() } withDependencies: {
            $0.viewsClient.observe = feed.observe
        }
        await store.send(.task)
        await store.receive(.loaded(document)) {
            $0.document = document
            $0.offered = true
        }
    }

    func testAPeriodShowsChosenAtOnceAndTheFiguresFollowWhenTheDaemonAnswers() async throws {
        let week = try metrics()
        let month = try metrics(key: Self.month)
        var state = MetricsFeature.State(key: ViewKey(rawValue: Self.week))
        state.document = week
        state.offered = true
        let store = TestStore(initialState: state) { MetricsFeature() } withDependencies: {
            $0.viewsClient.observe = { _ in AsyncThrowingStream { _ in } }
        }
        let period = try section(week, "period")
        XCTAssertTrue(store.state.isChoice(period))
        XCTAssertTrue(store.state.isChosen(try row(period, "period:7d"), in: period))

        await store.send(.choose(row: "period:30d", key: ViewKey(rawValue: Self.month))) {
            $0.key = ViewKey(rawValue: Self.month)
            $0.chosen = "period:30d"
        }
        XCTAssertTrue(store.state.isChosen(try row(period, "period:30d"), in: period), "the tapped period is marked before the daemon answers")
        XCTAssertFalse(store.state.isChosen(try row(period, "period:7d"), in: period))
        let compare = try section(week, "compare")
        XCTAssertTrue(store.state.isChosen(try row(compare, "compare:previous"), in: compare), "the other choice keeps its mark")
        XCTAssertTrue(store.state.loading)

        await store.send(.loaded(week))
        await store.send(.loaded(month)) {
            $0.document = month
            $0.chosen = nil
        }
        XCTAssertFalse(store.state.loading)
        await store.send(.stop)
    }

    func testAChoiceTheDaemonCannotShowGoesBackToWhatIsOnScreen() async throws {
        let week = try metrics()
        let feed = FeedStub([week])
        var state = MetricsFeature.State(key: ViewKey(rawValue: Self.week))
        state.document = week
        state.offered = true
        let store = TestStore(initialState: state) { MetricsFeature() } withDependencies: {
            $0.viewsClient.observe = feed.observe
        }
        await store.send(.choose(row: "period:30d", key: ViewKey(rawValue: Self.month))) {
            $0.key = ViewKey(rawValue: Self.month)
            $0.chosen = "period:30d"
        }
        await store.receive(.missing) {
            $0.key = ViewKey(rawValue: Self.week)
            $0.chosen = nil
        }
        await store.receive(.loaded(week))
        XCTAssertTrue(store.state.offered)
    }
}
