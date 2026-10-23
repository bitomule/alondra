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

    func testALoadFailureKeepsTheCachedDocumentForTheConnectionNotice() async throws {
        let document = try makeDocument(view: "schedules", key: "schedules", sections: #"[{"id":"schedules","collapsed":false,"items":[]}]"#)
        var state = ViewsScreenFeature.State(key: .schedules, owner: "schedules-offline-test")
        state.document = document
        state.isPresented = true
        let store = TestStore(initialState: state) { ViewsScreenFeature() }

        await store.send(.failed("offline")) { $0.error = "offline" }
        XCTAssertEqual(store.state.document, document)
    }
}
