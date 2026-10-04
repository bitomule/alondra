import AlondraContract
import AlondraFeed
import AlondraFeatures
import ComposableArchitecture
import Foundation
import XCTest

@MainActor
final class ViewsScreenFeatureTests: XCTestCase {
    private func decode(_ json: String) throws -> ViewDocument {
        try JSONDecoder().decode(ViewDocument.self, from: Data(json.utf8))
    }

    private let status = #"{"version":"s1","summary":"Ready","badge":0,"counts":[],"gauges":[],"orchestrator":{"text":"idle","tone":"neutral"}}"#

    private func makeDocument(view: String, key: String, sections: String = "[]", extra: String = "") throws -> ViewDocument {
        try decode(#"{"protocol":1,"view":"\#(view)","key":"\#(key)","client":"ios","version":"v1","cursor":"c1","data_epoch":"epoch","status":\#(status),"sections":\#(sections)\#(extra)}"#)
    }

    func testScheduleScreenOpensItsEditableFormWithServerDefaults() async throws {
        let sections = #"[{"id":"form","collapsed":false,"items":[{"type":"form","id":"schedule","fields":[{"id":"repeat","label":"Repeat","kind":{"kind":"choice","items":[{"id":"daily","label":"Daily"}]},"value":"daily","required":true,"reload_on_change":false},{"id":"time","label":"Time","kind":{"kind":"time"},"value":"09:00","required":true,"reload_on_change":false}],"submit":{"id":"sched_save:view@1","label":"Save Schedule","style":"primary","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":true}}]}]"#
        let document = try makeDocument(view: "schedule", key: "schedule:new", sections: sections)
        let form = try XCTUnwrap(document.sections.first?.items.first.flatMap { item -> AlondraContract.Form? in
            if case let .form(form) = item { form } else { nil }
        })
        let store = TestStore(initialState: ViewsScreenFeature.State(key: .newSchedule(), owner: "schedule-editor-test")) {
            ViewsScreenFeature()
        }

        await store.send(.loaded(.document(document))) {
            $0.document = document
            $0.action.dataEpoch = "epoch"
            $0.form = form
            $0.action.action = form.submit
            $0.action.fields = ["repeat": .string("daily"), "time": .string("09:00")]
            $0.action.requiresAttachments = false
        }
    }

    func testExplicitRefreshOmitsTheKnownViewVersion() async throws {
        var state = ViewsScreenFeature.State(key: .tasks, owner: "refresh-test")
        state.document = try makeDocument(view: "tasks", key: "tasks")
        let store = TestStore(initialState: state) { ViewsScreenFeature() } withDependencies: {
            $0.viewsClient.openScreen = { _, _ in }
            $0.viewsClient.load = { key, known in
                XCTAssertEqual(key, .tasks)
                XCTAssertNil(known)
                return .unchanged(cursor: "c2")
            }
        }
        await store.send(.refresh) { $0.loading = true }
        await store.receive(\.loaded) { $0.loading = false }
    }

    func testFleetLoadFailureKeepsCachedDocumentForConnectionNotice() async throws {
        let document = try makeDocument(view: "fleet", key: "fleet", sections: #"[{"id":"agents","collapsed":false,"items":[]}]"#)
        var state = ViewsScreenFeature.State(key: .fleet, owner: "fleet-offline-test")
        state.document = document
        state.isPresented = true
        let store = TestStore(initialState: state) { ViewsScreenFeature() }

        await store.send(.failed("offline")) { $0.error = "offline" }
        XCTAssertEqual(store.state.document, document)
    }

    private func searchDocument(empty: Bool) throws -> ViewDocument {
        let content = empty
            ? #","empty":{"text":"No results found","tone":"neutral"}"#
            : ""
        let rows = empty
            ? "[]"
            : #"[{"id":"results","collapsed":false,"items":[{"type":"row","id":"node-a","icon":"node","title":"Node A","depth":0,"struck":false,"children":[]}]}]"#
        return try makeDocument(view: "search", key: "search?q=node", sections: rows, extra: content)
    }

    private func fleetSearchStore(
        clock: TestClock<Duration>,
        load: @escaping @Sendable (ViewKey, String?) async throws -> ViewFetch
    ) -> TestStoreOf<ViewsScreenFeature> {
        var state = ViewsScreenFeature.State(key: .fleet, owner: "fleet-search-test")
        state.isPresented = true
        return TestStore(initialState: state) { ViewsScreenFeature() } withDependencies: {
            $0.continuousClock = clock
            $0.viewsClient.openScreen = { _, _ in }
            $0.viewsClient.load = load
        }
    }

    func testFleetSearchShowsTheMatches() async throws {
        let clock = TestClock()
        let document = try searchDocument(empty: false)
        let store = fleetSearchStore(clock: clock) { key, _ in
            XCTAssertEqual(key.rawValue, "search?q=node")
            return .document(document)
        }
        await store.send(.searchQueryChanged("node")) { $0.searchQuery = "node"; $0.searchLoading = true }
        await clock.advance(by: .milliseconds(250))
        await store.receive(\.searchLoaded) { $0.searchDocument = document; $0.searchLoading = false }
        XCTAssertNil(store.state.searchError)
    }

    func testFleetSearchReportsAnEmptyResult() async throws {
        let clock = TestClock()
        let document = try searchDocument(empty: true)
        let store = fleetSearchStore(clock: clock) { _, _ in .document(document) }
        await store.send(.searchQueryChanged("node")) { $0.searchQuery = "node"; $0.searchLoading = true }
        await clock.advance(by: .milliseconds(250))
        await store.receive(\.searchLoaded) { $0.searchDocument = document; $0.searchLoading = false }
        XCTAssertEqual(store.state.searchDocument?.empty?.text, "No results found")
        XCTAssertNil(store.state.searchError)
    }

    func testFleetSearchShowsAnErrorWhenItFails() async throws {
        struct Offline: Error {}
        let clock = TestClock()
        let store = fleetSearchStore(clock: clock) { _, _ in throw Offline() }
        await store.send(.searchQueryChanged("node")) { $0.searchQuery = "node"; $0.searchLoading = true }
        await clock.advance(by: .milliseconds(250))
        await store.receive(\.searchFailed) {
            $0.searchLoading = false
            $0.searchError = String(localized: "Couldn’t load this from your Mac. Try again.")
        }
        await store.send(.searchQueryChanged("nodes")) { $0.searchQuery = "nodes"; $0.searchLoading = true; $0.searchError = nil }
        await store.send(.searchQueryChanged("")) { $0.searchQuery = ""; $0.searchLoading = false }
    }
}
