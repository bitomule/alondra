import AlondraContract
import AlondraDesign
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import XCTest

@MainActor
final class PRReviewFeatureTests: XCTestCase {
    private let key = ViewKey.pullRequest("github.com/example/repo#12")

    private nonisolated static let diffText = """
    diff --git a/Sources/App.swift b/Sources/App.swift
    --- a/Sources/App.swift
    +++ b/Sources/App.swift
    @@ -1,2 +1,2 @@
    -let a = 1
    +let a = 2
     let b = 3
    diff --git a/Cargo.lock b/Cargo.lock
    --- a/Cargo.lock
    +++ b/Cargo.lock
    @@ -1,1 +1,1 @@
    -version = 1
    +version = 2
    """

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

    private func document(adding actions: [String] = [], head: String? = nil) throws -> ViewDocument {
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
            if let head {
                var refs = items(of: "diff", in: object)
                refs[0]["digest"] = head
                replaceItems(of: "diff", in: &object, with: refs)
            }
        })
    }

    private func store(
        requests: LockIsolated<[ActionRequest]> = LockIsolated([]),
        reply: String = #"{"protocol":1,"outcome":"applied","message":"Done"}"#,
        diffs: LockIsolated<[String?]> = LockIsolated([]),
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
            $0.viewsClient.fetchDiff = { ref in
                diffs.withValue { $0.append(ref.digest) }
                return FetchedDiff(text: Self.diffText)
            }
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
                       ["example/repo", "main", "feature/compact-schedule-editor", "4 files · +120 −30"])
        XCTAssertEqual(review.checks.map(\.name), ["Build", "Unit tests", "Lint"])
        XCTAssertEqual(review.checks.map(\.state), [.pass, .pass, .pending])
        XCTAssertTrue(review.description?.text.hasPrefix("Replaces the long schedule form") == true)
        XCTAssertEqual(review.diff?.digest, "abcdef12")
        XCTAssertEqual(review.pageURL, URL(string: "https://github.com/example/repo/pull/12"))
        XCTAssertEqual(review.requestChanges?.comments?.kind, .diffLines)
        XCTAssertEqual(review.merge?.options.map(\.id), ["merge", "squash"])
        XCTAssertNotNil(review.resolveConflicts)
    }

    func testOnlyAPageOnTheHostIsEverOpened() {
        XCTAssertNotNil(PRReview.pageURL("https://github.com/o/r/pull/1"))
        XCTAssertNil(PRReview.pageURL("http://github.com/o/r/pull/1"))
        XCTAssertNil(PRReview.pageURL("https://evil.example/github.com"))
        XCTAssertNil(PRReview.pageURL("https://notgithub.com/o/r/pull/1"))
    }

    func testShowsTheDiffOfTheCurrentHeadParsedAndFoldsGeneratedFiles() async throws {
        let diffs = LockIsolated<[String?]>([])
        let store = store(diffs: diffs)

        await store.send(.screen(.loaded(try document())))
        await store.receive(\.diffLoaded)

        XCTAssertEqual(diffs.value, ["abcdef12"])
        XCTAssertEqual(store.state.diff, .loaded(UnifiedDiff.parse(Self.diffText)))
        XCTAssertEqual(store.state.collapsed, ["Cargo.lock"])
        XCTAssertEqual(store.state.actions.dataEpoch, "test-epoch")
    }

    func testAnUnchangedHeadIsNotFetchedAgain() async throws {
        let diffs = LockIsolated<[String?]>([])
        let store = store(diffs: diffs)

        await store.send(.screen(.loaded(try document())))
        await store.receive(\.diffLoaded)
        await store.send(.screen(.loaded(try Fixtures.document("pull-request-ios", version: "v2"))))

        XCTAssertEqual(diffs.value, ["abcdef12"])
    }

    func testAMovedHeadDropsTheLineCommentsAndFetchesItsDiff() async throws {
        let diffs = LockIsolated<[String?]>([])
        let store = store(diffs: diffs)
        await store.send(.screen(.loaded(try document())))
        await store.receive(\.diffLoaded)
        await store.send(.comment(anchor: "Sources/App.swift:new:1", ActionComment(anchor: "Sources/App.swift:new:1", text: "Why 2?")))
        XCTAssertEqual(store.state.comments.count, 1)

        await store.send(.screen(.loaded(try document(head: "feed1234"))))
        await store.receive(\.diffLoaded)

        XCTAssertEqual(diffs.value, ["abcdef12", "feed1234"])
        XCTAssertTrue(store.state.comments.isEmpty)
        XCTAssertEqual(store.state.diffDigest, "feed1234")
    }

    func testADiffThatArrivesForAnOldHeadIsIgnored() async throws {
        let store = store()
        await store.send(.screen(.loaded(try document())))
        await store.receive(\.diffLoaded)
        let shown = store.state.diff

        await store.send(.diffLoaded(digest: "old0000", []))
        await store.send(.diffFailed(digest: "old0000", "late"))

        XCTAssertEqual(store.state.diff, shown)
    }

    func testAFailedDiffCanBeTriedAgain() async throws {
        let attempts = LockIsolated(0)
        let store = TestStore(initialState: PRReviewFeature.State(key: key)) { PRReviewFeature() } withDependencies: {
            $0.viewsClient.fetchDiff = { _ in
                if attempts.withValue({ $0 += 1; return $0 }) == 1 { throw ViewsClientError.missingView }
                return FetchedDiff(text: Self.diffText, truncated: true)
            }
        }
        store.exhaustivity = .off

        await store.send(.screen(.loaded(try document())))
        await store.receive(\.diffFailed)
        XCTAssertEqual(store.state.diff, .failed("Couldn’t load the diff. Try again."))
        await store.send(.retryDiff)
        await store.receive(\.diffLoaded)

        XCTAssertEqual(store.state.diff, .loaded(UnifiedDiff.parse(Self.diffText)))
        XCTAssertTrue(store.state.diffTruncated, "a diff the Mac cut says so")
    }

    func testAViewWithoutADiffShowsNone() async throws {
        let store = store()
        let document = try Fixtures.document("pull-request-ios", edit: { object in
            object["sections"] = (object["sections"] as? [[String: Any]] ?? []).filter { $0["id"] as? String != "diff" }
        })

        await store.send(.screen(.loaded(document)))

        XCTAssertEqual(store.state.diff, .unavailable)
    }

    func testRequestChangesSendsTheLineCommentsAndTheNote() async throws {
        let requests = LockIsolated<[ActionRequest]>([])
        let store = store(requests: requests)
        await store.send(.screen(.loaded(try document())))
        await store.receive(\.diffLoaded)
        let second = ActionComment(anchor: "Sources/App.swift:new:2", text: "Keep b")
        let first = ActionComment(anchor: "Sources/App.swift:new:1", text: "Why 2?")
        await store.send(.comment(anchor: second.anchor, second))
        await store.send(.comment(anchor: first.anchor, first))
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
        XCTAssertEqual(request.input.comments, [first, second])
    }

    func testAMergeTheServerHoldsBackOpensNothingAndSendsNothing() async throws {
        let requests = LockIsolated<[ActionRequest]>([])
        let store = store(requests: requests)
        await store.send(.screen(.loaded(try document())))
        await store.receive(\.diffLoaded)
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
        await store.receive(\.diffLoaded)
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
        await store.receive(\.diffLoaded)
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
        await store.receive(\.diffLoaded)

        await store.send(.perform(try XCTUnwrap(store.state.review?.resolveConflicts)))
        await store.receive(\.actions.sent)
        await store.receive(\.delegate.handOff)
        await store.finish()

        XCTAssertTrue(dismissed.value)
        XCTAssertFalse(performed.value, "the app delivers it and reports the outcome; the screen never waits")
    }

    func testFoldingOneFileOrAllOfThem() async throws {
        let store = store()
        await store.send(.screen(.loaded(try document())))
        await store.receive(\.diffLoaded)
        XCTAssertEqual(store.state.collapsed, ["Cargo.lock"])

        await store.send(.toggleFile("Sources/App.swift"))
        XCTAssertEqual(store.state.collapsed, ["Cargo.lock", "Sources/App.swift"])
        await store.send(.toggleAllFiles)
        XCTAssertEqual(store.state.collapsed, [])
        await store.send(.toggleAllFiles)
        XCTAssertEqual(store.state.collapsed, ["Cargo.lock", "Sources/App.swift"])
    }

    func testACommentCanBeEditedAndRemoved() async {
        let store = store()
        let comment = ActionComment(anchor: "a", text: "one")

        await store.send(.comment(anchor: "a", comment))
        await store.send(.comment(anchor: "a", ActionComment(anchor: "a", text: "two")))
        XCTAssertEqual(store.state.comments["a"]?.text, "two")
        await store.send(.comment(anchor: "a", nil))
        XCTAssertTrue(store.state.comments.isEmpty)
    }

    func testALinkToAScreenOpensItInTheTabAndAWebLinkOpensNothing() async {
        let store = store()

        await store.send(.follow(.view(key: "task:t-1")))
        await store.receive(\.delegate.open, ViewKey(rawValue: "task:t-1"))
        await store.send(.follow(.url(url: "https://github.com/o/r/pull/1")))
    }

    func testAnAttachmentOpensAPreviewAndClosesIt() async throws {
        let attachment = try ContractCodec.decode(AlondraContract.Attachment.self, from: Data(
            #"{"id":"a1","name":"shot.png","kind":"image","size":4,"fetch":"/attachments/a1"}"#.utf8))
        let store = TestStore(initialState: PRReviewFeature.State(key: key)) { PRReviewFeature() } withDependencies: {
            $0.viewsClient.fetchAttachment = { _ in Data([1, 2, 3, 4]) }
        }
        store.exhaustivity = .off

        await store.send(.showAttachment(attachment))
        await store.receive(\.attachmentLoaded)
        XCTAssertEqual(store.state.attachment?.data, Data([1, 2, 3, 4]))
        await store.send(.closeAttachment)
        XCTAssertNil(store.state.attachment)
    }
}
