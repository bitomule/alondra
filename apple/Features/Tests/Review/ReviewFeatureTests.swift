import AlondraAttachments
import AlondraContract
import AlondraDesign
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import Foundation
import XCTest

final class ReviewFeatureTests: XCTestCase {
    private let deliveryKey = ViewKey.card(Fixtures.deliveryID)

    @MainActor
    private func store(
        _ document: ViewDocument, feed: FeedStub = FeedStub(),
        content: (@Sendable (ContentRef) async throws -> String)? = nil,
        dismissed: LockIsolated<Int> = LockIsolated(0)
    ) -> TestStoreOf<ReviewFeature> {
        var state = ReviewFeature.State(key: ViewKey(rawValue: document.key))
        state.screen.document = document
        state.screen.phase = .loaded
        state.actions.dataEpoch = document.dataEpoch
        let store = TestStore(initialState: state) { ReviewFeature() } withDependencies: {
            $0.viewsClient.observe = feed.observe
            $0.viewsClient.perform = feed.perform()
            if let content { $0.viewsClient.fetchContent = content }
            $0.dismiss = DismissEffect { dismissed.withValue { $0 += 1 } }
        }
        store.exhaustivity = .off
        return store
    }

    private func labels(_ store: TestStoreOf<ReviewFeature>) -> [String] {
        store.state.subject?.barActions.map(\.label) ?? []
    }

    // MARK: What the review offers

    @MainActor
    func testADeliveryOffersChangesAndAcceptAndNeverMarkAsRead() async throws {
        let withRead = try Fixtures.card("card-delivery") { item in
            var actions = try XCTUnwrap(item["actions"] as? [[String: Any]])
            actions.removeAll()
            actions.append(["id": "read:\(Fixtures.deliveryID)@1", "label": "Mark as read", "style": "secondary",
                            "presentation": "button", "input": ["kind": "none"], "attachments": false, "enabled": true])
            item["actions"] = actions
        }
        let store = store(withRead)
        XCTAssertEqual(labels(store), [], "a delivery never offers Mark as Read")
        let normal = self.store(try Fixtures.liveDelivery())
        XCTAssertEqual(labels(normal), ["Request changes", "Accept"])
    }

    @MainActor
    func testAReportKeepsItsMarkAsRead() async throws {
        let store = store(try Fixtures.card(Fixtures.informationID))
        XCTAssertEqual(labels(store), ["Mark as read", "Reply"])
    }

    @MainActor
    func testOnlyAClaudeDeliveryOffersToRescueComments() async throws {
        let delivery = store(try Fixtures.liveDelivery())
        let subject = try XCTUnwrap(delivery.state.subject)
        let changes = try XCTUnwrap(subject.actions.first { $0.id?.hasPrefix("changes:") == true })
        XCTAssertTrue(subject.isRescue(changes))

        let plan = store(try Fixtures.planCard())
        let planSubject = try XCTUnwrap(plan.state.subject)
        let review = try XCTUnwrap(planSubject.actions.first { $0.id?.hasPrefix("changes:") == true })
        XCTAssertFalse(planSubject.isRescue(review), "a plan to review is not a delivery to rescue")
    }

    @MainActor
    func testAQuestionShowsItsOptionsInThePageAndNotInTheBar() async throws {
        let store = store(try Fixtures.card(Fixtures.questionID))
        XCTAssertEqual(store.state.subject?.options?.options.map(\.label), ["Ship the concise copy", "Keep the warmer copy"])
        XCTAssertEqual(labels(store), ["Need more context"])
    }

    // MARK: Block comments

    @MainActor
    func testACommentIsKeptPerBlockAndAnEmptyOneRemovesIt() async throws {
        let store = store(try Fixtures.liveDelivery())
        let block = try XCTUnwrap(store.state.blocks.first { $0.isCommentable })

        await store.send(.blockTapped(anchor: block.draftKey, quote: block.plainText))
        XCTAssertEqual(store.state.composing?.text, "")
        await store.send(.composingChanged("  Needs a number  "))
        await store.send(.commentSaved)
        XCTAssertNil(store.state.composing)
        XCTAssertEqual(store.state.comments[block.draftKey]?.text, "Needs a number")
        XCTAssertEqual(store.state.comments[block.draftKey]?.quote, block.plainText)

        await store.send(.blockTapped(anchor: block.draftKey, quote: block.plainText))
        XCTAssertEqual(store.state.composing?.text, "Needs a number", "editing starts from the saved comment")
        await store.send(.composingChanged("   "))
        await store.send(.commentSaved)
        XCTAssertNil(store.state.comments[block.draftKey])
    }

    @MainActor
    func testDeletingAndCancellingACommentLeaveTheRestAlone() async throws {
        let store = store(try Fixtures.liveDelivery())
        let blocks = store.state.blocks.filter(\.isCommentable)
        let first = try XCTUnwrap(blocks.first), second = try XCTUnwrap(blocks.dropFirst().first)
        for block in [first, second] {
            await store.send(.blockTapped(anchor: block.draftKey, quote: block.plainText))
            await store.send(.composingChanged("note on \(block.plainText)"))
            await store.send(.commentSaved)
        }

        await store.send(.blockTapped(anchor: first.draftKey, quote: first.plainText))
        await store.send(.commentDeleted)
        XCTAssertEqual(Set(store.state.comments.keys), [second.draftKey])

        await store.send(.blockTapped(anchor: second.draftKey, quote: second.plainText))
        await store.send(.composingChanged("something else"))
        await store.send(.commentCancelled)
        XCTAssertEqual(store.state.comments[second.draftKey]?.text, "note on \(second.plainText)")
    }

    // MARK: Sending

    @MainActor
    func testRequestingChangesIsDisabledUntilThereIsFeedbackThenSendsTheCommentsInReadingOrder() async throws {
        let feed = FeedStub()
        let dismissed = LockIsolated(0)
        let store = store(try Fixtures.liveDelivery(), feed: feed, dismissed: dismissed)
        let changes = try XCTUnwrap(store.state.subject?.actions.first { $0.id?.hasPrefix("changes:") == true })
        XCTAssertFalse(store.state.isSendable(changes))
        await store.send(.actionTapped(changes))
        XCTAssertEqual(feed.requests, [], "nothing to send yet")

        let blocks = store.state.blocks.filter(\.isCommentable)
        let first = try XCTUnwrap(blocks.first), last = try XCTUnwrap(blocks.last)
        for block in [last, first] {
            await store.send(.blockTapped(anchor: block.draftKey, quote: block.plainText))
            await store.send(.composingChanged("note on \(block.plainText)"))
            await store.send(.commentSaved)
        }
        await store.send(.generalChanged("Overall: good"))
        XCTAssertTrue(store.state.isSendable(changes))
        await store.send(.actionTapped(changes))
        await store.skipReceivedActions()

        let request = try XCTUnwrap(feed.requests.first)
        XCTAssertEqual(request.actionId, changes.id)
        XCTAssertEqual(request.input.comments?.map(\.anchor), [first.draftKey, last.draftKey])
        XCTAssertEqual(request.input.text, "Overall: good")
        XCTAssertEqual(dismissed.value, 1, "the review closes once the server applied it")
        XCTAssertTrue(store.state.comments.isEmpty)
        XCTAssertEqual(store.state.general, "")
    }

    @MainActor
    func testBlockCommentsAloneSatisfyAChangeRequestThatRequiresText() async throws {
        let feed = FeedStub()
        let store = store(try Fixtures.liveDelivery(), feed: feed)
        let changes = try XCTUnwrap(store.state.subject?.actions.first { $0.id?.hasPrefix("changes:") == true })
        guard case .text(_, let required, _) = changes.input else { return XCTFail("the server asks for text") }
        XCTAssertTrue(required)
        let block = try XCTUnwrap(store.state.blocks.first { $0.isCommentable })
        await store.send(.blockTapped(anchor: block.draftKey, quote: block.plainText))
        await store.send(.composingChanged("Needs a number"))
        await store.send(.commentSaved)

        await store.send(.actionTapped(changes))
        await store.skipReceivedActions()
        XCTAssertEqual(feed.requests.first?.input.comments?.map(\.text), ["Needs a number"])
        XCTAssertNil(feed.requests.first?.input.text.flatMap { $0.isEmpty ? nil : $0 })
    }

    @MainActor
    func testAcceptingNeedsNoFeedbackAndClosesTheReview() async throws {
        let feed = FeedStub()
        let dismissed = LockIsolated(0)
        let store = store(try Fixtures.liveDelivery(), feed: feed, dismissed: dismissed)
        let accept = try XCTUnwrap(store.state.subject?.actions.first { $0.id?.hasPrefix("accept:") == true })

        await store.send(.actionTapped(accept))
        await store.skipReceivedActions()
        XCTAssertEqual(feed.requests.map(\.actionId), [accept.id])
        XCTAssertEqual(dismissed.value, 1)
    }

    @MainActor
    func testAFailedSendKeepsTheCommentsAndTheReviewOpen() async throws {
        let dismissed = LockIsolated(0)
        let feed = FeedStub()
        var state = ReviewFeature.State(key: deliveryKey)
        state.screen.document = try Fixtures.liveDelivery()
        let store = TestStore(initialState: state) { ReviewFeature() } withDependencies: {
            $0.viewsClient.perform = feed.perform(#"{"protocol":1,"outcome":"rejected","message":"This card has changed"}"#)
            $0.dismiss = DismissEffect { dismissed.withValue { $0 += 1 } }
        }
        store.exhaustivity = .off
        let block = try XCTUnwrap(store.state.blocks.first { $0.isCommentable })
        await store.send(.blockTapped(anchor: block.draftKey, quote: block.plainText))
        await store.send(.composingChanged("keep me"))
        await store.send(.commentSaved)
        let changes = try XCTUnwrap(store.state.subject?.actions.first { $0.id?.hasPrefix("changes:") == true })

        await store.send(.actionTapped(changes))
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.comments[block.draftKey]?.text, "keep me")
        XCTAssertEqual(dismissed.value, 0)
        XCTAssertEqual(store.state.actions.error, "This card has changed")
    }

    @MainActor
    func testPickingAnOptionOnAQuestionAnswersIt() async throws {
        let feed = FeedStub()
        let store = store(try Fixtures.card(Fixtures.questionID), feed: feed)
        let answer = try XCTUnwrap(store.state.subject?.options)

        await store.send(.optionPicked(answer, optionID: "warm"))
        await store.skipReceivedActions()
        XCTAssertEqual(feed.requests.first?.optionId, "warm")
    }

    @MainActor
    func testALinkActionOpensItsScreenInsteadOfSending() async throws {
        let feed = FeedStub()
        let store = store(try Fixtures.liveDelivery(), feed: feed)
        var action = try XCTUnwrap(store.state.subject?.actions.first)
        action = try ContractCodec.decode(AlondraContract.Action.self, from: Data(
            #"{"label":"Open","style":"secondary","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":true,"link":{"kind":"view","key":"task:t-1"}}"#.utf8))

        await store.send(.actionTapped(action))
        await store.receive(\.delegate.open, ViewKey(rawValue: "task:t-1"))
        XCTAssertEqual(feed.requests, [])
    }

    // MARK: Document text

    @MainActor
    func testADocumentsTextIsFetchedOnceByDigestAndRefetchedWhenItChanges() async throws {
        let fetched = LockIsolated<[String?]>([])
        let store = store(try Fixtures.documentRevision(digest: "d1"), content: { reference in
            fetched.withValue { $0.append(reference.digest) }
            return "# Plan\n\nFirst paragraph.\n\nSecond paragraph."
        })

        await store.send(.screen(.loaded(try Fixtures.documentRevision(digest: "d1"))))
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.markdown, "# Plan\n\nFirst paragraph.\n\nSecond paragraph.")
        XCTAssertEqual(store.state.blocks.count, 3)
        XCTAssertFalse(store.state.contentPending)

        await store.send(.screen(.loaded(try Fixtures.documentRevision(digest: "d1"))))
        XCTAssertEqual(fetched.value, ["d1"], "the same digest is not fetched twice")

        await store.send(.screen(.loaded(try Fixtures.documentRevision(digest: "d2"))))
        await store.skipReceivedActions()
        XCTAssertEqual(fetched.value, ["d1", "d2"])
    }

    @MainActor
    func testADocumentThatCouldNotBeFetchedCanBeRetried() async throws {
        struct Offline: Error {}
        let attempts = LockIsolated(0)
        let store = store(try Fixtures.documentRevision(), content: { _ in
            let attempt = attempts.withValue { $0 += 1; return $0 }
            if attempt == 1 { throw Offline() }
            return "Recovered text"
        })

        await store.send(.screen(.loaded(try Fixtures.documentRevision())))
        await store.skipReceivedActions()
        XCTAssertTrue(store.state.contentFailed)
        XCTAssertEqual(store.state.markdown, "")

        await store.send(.screen(.loaded(try Fixtures.documentRevision())))
        await store.skipReceivedActions()
        XCTAssertFalse(store.state.contentFailed)
        XCTAssertEqual(store.state.markdown, "Recovered text")
    }

    @MainActor
    func testADocumentOffersApproveAndRequestChangesFromItsActionBar() async throws {
        let store = store(try Fixtures.documentRevision())
        XCTAssertEqual(store.state.subject?.icon, .document)
        XCTAssertEqual(store.state.subject?.title, "Release plan")
        XCTAssertEqual(labels(store), ["Request changes", "Approve"])
        XCTAssertTrue(store.state.canComment)
    }

    @MainActor
    func testAHeldPlanIsReadInFullCommentedAndApprovedOrSentBackFromTheBar() async throws {
        let feed = FeedStub()
        let store = store(try Fixtures.heldPlanCard(), feed: feed)
        XCTAssertEqual(store.state.markdown, "# Ship the parser\n\n1. Split the lexer\n2. Add tests")
        XCTAssertEqual(labels(store), ["Request changes", "Approve"])
        XCTAssertNil(store.state.subject?.options, "Approve and Request changes are not repeated in the page")
        XCTAssertTrue(store.state.acceptsFeedback)

        let block = try XCTUnwrap(store.state.blocks.last { $0.isCommentable })
        await store.send(.blockTapped(anchor: block.draftKey, quote: block.plainText))
        await store.send(.composingChanged("Keep the old tests"))
        await store.send(.commentSaved)
        let changes = try XCTUnwrap(store.state.subject?.barActions.first)
        await store.send(.actionTapped(changes))
        await store.skipReceivedActions()
        XCTAssertEqual(feed.requests.first?.actionId, "changes:hold-1@1")
        XCTAssertEqual(feed.requests.first?.input.comments?.map(\.text), ["Keep the old tests"])

        let approving = self.store(try Fixtures.heldPlanCard(), feed: feed)
        let approve = try XCTUnwrap(approving.state.subject?.barActions.last)
        await approving.send(.actionTapped(approve))
        await approving.skipReceivedActions()
        XCTAssertEqual(feed.requests.last?.actionId, "accept:hold-1@1")
    }

    @MainActor
    func testAPlanTheNodeAskedAboutOffersItsOptionsAndRequestChangesWithComments() async throws {
        let feed = FeedStub()
        let store = store(try Fixtures.planQuestionCard(), feed: feed, content: { _ in "# Plan\n\nOne task per node." })
        await store.send(.screen(.loaded(try Fixtures.planQuestionCard())))
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.subject?.icon, .document)
        XCTAssertEqual(store.state.subject?.options?.options.map(\.label), ["Approve; messaging in its own PR"])
        XCTAssertEqual(labels(store), ["Request changes"])
        XCTAssertTrue(store.state.canComment)

        let block = try XCTUnwrap(store.state.blocks.first { $0.isCommentable })
        await store.send(.blockTapped(anchor: block.draftKey, quote: block.plainText))
        await store.send(.composingChanged("Split it"))
        await store.send(.commentSaved)
        let changes = try XCTUnwrap(store.state.subject?.barActions.first)
        await store.send(.actionTapped(changes))
        await store.skipReceivedActions()
        XCTAssertEqual(feed.requests.first?.actionId, "changes:q-1@1")
        XCTAssertEqual(feed.requests.first?.input.comments?.map(\.text), ["Split it"])
    }

    @MainActor
    func testTheScreenObservesItsOwnKeyAndStopsWhenAsked() async throws {
        let feed = FeedStub([try Fixtures.liveDelivery()])
        var state = ReviewFeature.State(key: deliveryKey)
        state.actions.dataEpoch = ""
        let store = TestStore(initialState: state) { ReviewFeature() } withDependencies: {
            $0.viewsClient.observe = feed.observe
        }
        store.exhaustivity = .off

        await store.send(.task)
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.subject?.title, "Restore the native review experience")
        XCTAssertEqual(store.state.actions.dataEpoch, "test-epoch")
    }
}
