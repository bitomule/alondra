import AlondraContract
import AlondraDesign
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import XCTest

@MainActor
final class PRReviewFeatureTests: XCTestCase {
    private let key = ViewKey.pullRequest("github.com/example/repo#12")

    private static let mergeAction = """
    {"id":"merge:github.com/example/repo#12@3~abcdef12","label":"Merge","style":"primary","presentation":"menu",
     "options":[{"id":"merge","label":"Merge commit","style":"secondary","input":{"kind":"none"}},
                {"id":"squash","label":"squash","style":"secondary","input":{"kind":"none"}}],
     "input":{"kind":"none"},"attachments":false,"enabled":true}
    """

    private static let conflictsAction = """
    {"id":"conflicts:github.com/example/repo#12@3","label":"Resolve conflicts","style":"primary","presentation":"button",
     "input":{"kind":"none"},"attachments":false,"enabled":true}
    """

    private func document(adding actions: [String] = []) throws -> ViewDocument {
        try Fixtures.document("pull-request-ios", edit: { object in
            if !actions.isEmpty {
                var bar = items(of: "actions", in: object)
                var inner = bar[0]["actions"] as? [[String: Any]] ?? []
                for json in actions {
                    inner.append(try XCTUnwrap(JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any]))
                }
                bar[0]["actions"] = inner
                replaceItems(of: "actions", in: &object, with: bar)
            }
        })
    }

    private func store(
        requests: LockIsolated<[ActionRequest]> = LockIsolated([]),
        reply: String = #"{"protocol":1,"outcome":"applied","message":"Done"}"#,
        dismissed: LockIsolated<Bool> = LockIsolated(false)
    ) -> TestStoreOf<PRReviewFeature> {
        let store = TestStore(initialState: PRReviewFeature.State(key: key)) {
            CombineReducers {
                PRReviewFeature()
                Reduce { _, action in
                    if case .delegate(.handOff(let request)) = action { requests.withValue { $0.append(request) } }
                    return .none
                }
            }
        } withDependencies: {
            $0.uuid = .incrementing
            $0.dismiss = DismissEffect { dismissed.setValue(true) }
            $0.viewsClient.perform = { request in
                requests.withValue { $0.append(request) }
                return try Fixtures.response(reply)
            }
        }
        store.exhaustivity = .off
        return store
    }

    func testReadsTheViewIntoItsParts() throws {
        let review = PRReview(document: try document(adding: [Self.mergeAction, Self.conflictsAction]))

        XCTAssertEqual(review.title, "Add the compact schedule editor")
        XCTAssertEqual(review.facts.map(\.value),
                       ["example/repo", "main", "feature/compact-schedule-editor"])
        XCTAssertEqual(review.checks.map(\.name), ["Build", "Unit tests", "Lint"])
        XCTAssertEqual(review.checks.map(\.state), [.pass, .pass, .pending])
        XCTAssertTrue(review.description?.text.hasPrefix("Replaces the long schedule form") == true)
        XCTAssertEqual(review.pageURL, URL(string: "https://github.com/example/repo/pull/12"))
        XCTAssertNotNil(review.requestChanges)
        XCTAssertEqual(review.merge?.options.map(\.id), ["merge", "squash"])
        XCTAssertNotNil(review.resolveConflicts)
    }

    func testOnlyAPageOnTheHostIsEverOpened() {
        XCTAssertNotNil(PRReview.pageURL("https://github.com/o/r/pull/1"))
        XCTAssertNil(PRReview.pageURL("http://github.com/o/r/pull/1"))
        XCTAssertNil(PRReview.pageURL("https://evil.example/github.com"))
        XCTAssertNil(PRReview.pageURL("https://notgithub.com/o/r/pull/1"))
    }

    func testAnOlderDaemonsDiffSectionLoadsNothingMore() async throws {
        let store = TestStore(initialState: PRReviewFeature.State(key: key)) { PRReviewFeature() }
        let document = try Fixtures.document("pull-request-ios", edit: { object in
            var sections = object["sections"] as? [[String: Any]] ?? []
            sections.removeAll { $0["id"] as? String == "diff" }
            sections.append(["id": "diff", "title": "Changes", "collapsed": false, "items": [[
                "type": "content_ref", "kind": "diff", "title": "Changes", "size": 120, "digest": "abcdef12",
                "fetch": "/pull-requests/github.com%2Fexample%2Frepo%2312/diff", "commentable": false,
            ]]])
            object["sections"] = sections
        })

        await store.send(.screen(.loaded(document))) {
            $0.screen.document = document
            $0.screen.phase = .loaded
            $0.actions.dataEpoch = document.dataEpoch
        }
        XCTAssertEqual(store.state.review?.title, "Add the compact schedule editor")
    }

    func testRequestChangesSendsTheNote() async throws {
        let requests = LockIsolated<[ActionRequest]>([])
        let store = store(requests: requests)
        await store.send(.screen(.loaded(try document())))
        let changes = try XCTUnwrap(store.state.review?.requestChanges)

        await store.send(.perform(changes))
        await store.skipReceivedActions()
        XCTAssertTrue(store.state.isComposing)
        XCTAssertTrue(requests.value.isEmpty, "a text action waits for the sheet")

        await store.send(.actions(.textChanged("Please revisit")))
        await store.send(.actions(.submit))
        await store.skipReceivedActions()

        let request = try XCTUnwrap(requests.value.first)
        XCTAssertEqual(request.actionId, changes.id)
        XCTAssertEqual(request.input.text, "Please revisit")
    }

    func testAMergeTheServerHoldsBackOpensNothingAndSendsNothing() async throws {
        let requests = LockIsolated<[ActionRequest]>([])
        let store = store(requests: requests)
        await store.send(.screen(.loaded(try document())))
        let merge = try XCTUnwrap(store.state.review?.merge)
        XCTAssertFalse(merge.enabled, "the server's own fixture holds it back until a question is answered")

        await store.send(.perform(merge, optionID: "squash"))
        await store.skipReceivedActions(strict: false)
        XCTAssertNil(store.state.actions.action)
        XCTAssertNil(store.state.actions.confirmation)
        XCTAssertTrue(requests.value.isEmpty)
    }

    @MainActor
    func testMergeSendsTheChosenMethodWithNoConfirmation() async throws {
        let requests = LockIsolated<[ActionRequest]>([])
        let store = store(requests: requests)
        await store.send(.screen(.loaded(try document(adding: [Self.mergeAction]))))
        let merge = try XCTUnwrap(store.state.review?.merge)

        await store.send(.perform(merge, optionID: "squash"))
        await store.skipReceivedActions()

        XCTAssertNil(store.state.actions.confirmation)
        XCTAssertFalse(store.state.isComposing)
        XCTAssertEqual(requests.value.map(\.actionId), [merge.id])
        XCTAssertEqual(requests.value.first?.optionId, "squash")
    }

    func testHandingTheConflictsBackIsOneTap() async throws {
        let requests = LockIsolated<[ActionRequest]>([])
        let store = store(requests: requests)
        await store.send(.screen(.loaded(try document(adding: [Self.conflictsAction]))))
        let conflicts = try XCTUnwrap(store.state.review?.resolveConflicts)

        await store.send(.perform(conflicts))
        await store.skipReceivedActions()

        XCTAssertEqual(requests.value.map(\.actionId), [conflicts.id])
        XCTAssertNil(store.state.actions.confirmation)
    }

    func testAnActionClosesTheScreenAtOnceAndHandsItsRequestToTheApp() async throws {
        let dismissed = LockIsolated(false)
        let performed = LockIsolated(false)
        let store = store(dismissed: dismissed)
        store.dependencies.viewsClient.perform = { _ in
            performed.setValue(true)
            return try Fixtures.response(#"{"protocol":1,"outcome":"rejected","message":"Checks are failing"}"#)
        }
        await store.send(.screen(.loaded(try document(adding: [Self.conflictsAction]))))

        await store.send(.perform(try XCTUnwrap(store.state.review?.resolveConflicts)))
        await store.receive(\.actions.sent)
        await store.receive(\.delegate.handOff)
        await store.finish()

        XCTAssertTrue(dismissed.value)
        XCTAssertFalse(performed.value, "the app delivers it and reports the outcome; the screen never waits")
    }

    func testALinkToAScreenOpensItInTheTabAndAWebLinkOpensNothing() async {
        let store = store()

        await store.send(.follow(.view(key: "task:t-1")))
        await store.receive(\.delegate.open, ViewKey(rawValue: "task:t-1"))
        await store.send(.follow(.url(url: "https://github.com/o/r/pull/1")))
    }
}
