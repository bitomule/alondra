import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import Foundation
import XCTest

final class IssuePickerFeatureTests: XCTestCase {
    private func picker(_ rows: [(id: String, title: String, subtitle: String)]) throws -> ViewDocument {
        let data = try Fixtures.directory().appendingPathComponent("new-request-ios.json")
        var object = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: data)) as? [String: Any])
        object["key"] = "issue_picker"
        object["view"] = "issue_picker"
        object["title"] = "Issues"
        object["sections"] = [[
            "id": "issues", "collapsed": false,
            "items": rows.map { row -> [String: Any] in
                ["type": "row", "id": row.id, "icon": "mission", "title": row.title, "subtitle": row.subtitle,
                 "depth": 0, "struck": false]
            },
        ]]
        return try ContractCodec.decode(ViewDocument.self, from: JSONSerialization.data(withJSONObject: object))
    }

    @MainActor
    func testOpeningListsTheIssuesTheDaemonSent() async throws {
        let document = try picker([("me/App#4", "me/App#4", "Crash on launch")])
        let store = TestStore(initialState: IssuePickerFeature.State()) { IssuePickerFeature() } withDependencies: {
            $0.viewsClient.load = { key, _ in
                XCTAssertEqual(key, .issuePicker())
                return .document(document)
            }
        }

        await store.send(.task)
        await store.receive(\.loaded) {
            $0.rows = document.sections.flatMap(\.items).compactMap { if case .row(let row) = $0 { row } else { nil } }
            $0.loading = false
        }
        XCTAssertEqual(store.state.rows.map(\.id), ["me/App#4"])
    }

    @MainActor
    func testTypingWaitsForAPauseThenAsksTheDaemonToFilter() async throws {
        let clock = TestClock()
        let queries = LockIsolated<[ViewKey]>([])
        let pasted = try picker([("me/Other#3", "Use https://github.com/me/Other/issues/3", "me/Other#3")])
        let store = TestStore(initialState: IssuePickerFeature.State()) { IssuePickerFeature() } withDependencies: {
            $0.continuousClock = clock
            $0.viewsClient.load = { key, _ in
                queries.withValue { $0.append(key) }
                return .document(pasted)
            }
        }
        store.exhaustivity = .off

        await store.send(.queryChanged("https://github.com/me/Other/issues/3"))
        await store.send(.queryChanged("https://github.com/me/Other/issues/3 "))
        XCTAssertTrue(queries.value.isEmpty, "nothing is asked while the typing goes on")

        await clock.advance(by: .milliseconds(250))
        await store.receive(\.task)
        await store.receive(\.loaded)

        XCTAssertEqual(queries.value.count, 1)
        XCTAssertEqual(queries.value.first, .issuePicker(query: "https://github.com/me/Other/issues/3 "))
        XCTAssertEqual(store.state.rows.map(\.id), ["me/Other#3"], "a pasted link comes back as a row to choose")
    }

    @MainActor
    func testAFailedLoadSaysSoAndKeepsTheRows() async throws {
        struct Down: Error {}
        let store = TestStore(initialState: IssuePickerFeature.State()) { IssuePickerFeature() } withDependencies: {
            $0.viewsClient.load = { _, _ in throw Down() }
        }
        store.exhaustivity = .off

        await store.send(.task)
        await store.receive(\.failed)

        XCTAssertEqual(store.state.error, "Couldn’t load this from your Mac. Try again.")
        XCTAssertFalse(store.state.loading)
    }

    @MainActor
    func testChoosingARowReportsItsReference() async throws {
        let store = TestStore(initialState: IssuePickerFeature.State()) { IssuePickerFeature() }

        await store.send(.pick("me/App#4"))
        await store.receive(\.delegate.picked)
    }
}
