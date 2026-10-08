import AlondraAttachments
import AlondraContract
import AlondraDesign
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import Foundation
import XCTest

final class CardDetailFeatureTests: XCTestCase {
    private let planText = "# Plan\n\nBuild the form first.\n\nWire the sheet after."

    @MainActor
    private func store(
        _ document: ViewDocument, feed: FeedStub = FeedStub(),
        content: (@Sendable (ContentRef) async throws -> String)? = nil,
        dismissed: LockIsolated<Int> = LockIsolated(0)
    ) -> TestStoreOf<CardDetailFeature> {
        var state = CardDetailFeature.State(key: ViewKey(rawValue: document.key))
        state.screen.document = document
        state.screen.phase = .loaded
        state.actions.dataEpoch = document.dataEpoch
        let store = TestStore(initialState: state) {
            CombineReducers {
                CardDetailFeature()
                Reduce { _, action in
                    if case .delegate(.handOff(let request)) = action { feed.deliver(request) }
                    return .none
                }
            }
        } withDependencies: {
            $0.viewsClient.observe = feed.observe
            $0.viewsClient.perform = feed.perform()
            if let content { $0.viewsClient.fetchContent = content }
            $0.dismiss = DismissEffect { dismissed.withValue { $0 += 1 } }
        }
        store.exhaustivity = .off
        return store
    }

    /// The plan card with its text fetched, ready to be commented.
    @MainActor
    private func planStore(feed: FeedStub = FeedStub(), dismissed: LockIsolated<Int> = LockIsolated(0)) async throws
        -> TestStoreOf<CardDetailFeature> {
        let plan = try Fixtures.planCard()
        let text = planText
        let store = store(plan, feed: feed, content: { _ in text }, dismissed: dismissed)
        await store.send(.screen(.loaded(plan)))
        await store.skipReceivedActions()
        return store
    }

    private func labels(_ store: TestStoreOf<CardDetailFeature>) -> [String] {
        store.state.subject?.buttons.map(\.label) ?? []
    }

    // MARK: What the detail shows

    @MainActor
    func testACardSettledElsewhereClosesSayingWhyInsteadOfShowingIt() async throws {
        let dismissed = LockIsolated(0)
        let store = store(try Fixtures.card(Fixtures.questionID), dismissed: dismissed)
        let settled = try Fixtures.document("card-settled-ios-es")
        await store.send(.screen(.loaded(settled)))
        await store.receive(\.delegate.settled, "Ya respondida")
        await store.finish()
        XCTAssertEqual(dismissed.value, 1)
        XCTAssertEqual(DetailRoute.Action.cardDetail(.delegate(.settled("Ya respondida"))).delegate, .completed("Ya respondida"))
    }

    @MainActor
    func testAnActionTheServerHoldsBackCannotBePressedAndSendsNothing() async throws {
        let feed = FeedStub()
        let document = try Fixtures.document("pull-request-ios")
        let store = store(document, feed: feed)
        let merge = try XCTUnwrap(store.state.subject?.allActions.first { $0.id?.hasPrefix("merge:") == true })
        XCTAssertFalse(merge.enabled)
        XCTAssertFalse(store.state.isSendable(merge), "the button is drawn disabled, with its reason under it")

        await store.send(.actionTapped(merge))
        await store.skipReceivedActions(strict: false)
        XCTAssertNil(store.state.actions.action)
        XCTAssertEqual(feed.requests, [])
    }

    @MainActor
    func testTheDetailHasTheCardsActionsAndItsReplyAndTheServersSectionsInOrder() async throws {
        let store = store(try Fixtures.card(Fixtures.questionID))
        let subject = try XCTUnwrap(store.state.subject)

        XCTAssertEqual(subject.title, "Choose the welcome copy")
        XCTAssertEqual(subject.card?.kind, .decide)
        XCTAssertEqual(subject.sections.map(\.id), ["content", "context"])
        XCTAssertEqual(labels(store), ["★ Ship the concise copy", "Keep the warmer copy"])
        XCTAssertEqual(subject.menu.map(\.label), ["Write my own answer", "Request clarification"])
        XCTAssertEqual(subject.replyAction?.id, "reply:\(Fixtures.questionID)@1")
    }

    @MainActor
    func testAPullRequestsDiffOpensItsOwnScreenFromTheContent() async throws {
        let store = store(try Fixtures.card(Fixtures.pullRequestID))
        let reference = try XCTUnwrap(store.state.subject?.sections.first { $0.id == "diff" }?.items.compactMap {
            if case .contentRef(let reference) = $0 { reference } else { nil }
        }.first)

        XCTAssertEqual(CardDetailFeature.Subject.diffKey(of: reference), .pullRequest("github.com/example/repo#12"))
        XCTAssertEqual(store.state.subject?.sections.map(\.id), ["checks", "diff", "description", "context"])
        XCTAssertEqual(store.state.subject?.title, "Add the compact schedule editor")
    }

    @MainActor
    func testAPullRequestsDiffLoadsInsideTheDetail() async throws {
        let card = try Fixtures.card(Fixtures.pullRequestID)
        let fetched = LockIsolated<[String]>([])
        let store = store(card, content: { reference in
            fetched.withValue { $0.append(reference.fetch) }
            return "diff --git a/a.swift b/a.swift\n--- a/a.swift\n+++ b/a.swift\n@@ -1 +1 @@\n-old\n+new\n"
        })

        await store.send(.screen(.loaded(card)))
        await store.skipReceivedActions()

        XCTAssertEqual(fetched.value.count, 1)
        XCTAssertEqual(fetched.value.first?.hasPrefix("/pull-requests/"), true)
        XCTAssertEqual(store.state.diff?.count, 1)
        XCTAssertFalse(store.state.diffFailed)
    }

    @MainActor
    func testTheDetailOffersNoActionTheCardDoesNot() async throws {
        let card = try Fixtures.card(Fixtures.pullRequestID)
        let store = store(card)
        let onTheCard = card.sections.flatMap(\.items).compactMap { item -> Card? in
            if case .card(let card) = item { card } else { nil }
        }.first
        XCTAssertEqual(store.state.subject?.allActions.map(\.label),
                       ((onTheCard?.actions.prefix(3).map(\.label) ?? []) + (onTheCard?.more.map(\.label) ?? []) + ["Reply"]))
    }

    // MARK: Block comments

    @MainActor
    func testACommentIsKeptPerBlockAndAnEmptyOneRemovesIt() async throws {
        let store = try await planStore()
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
        let store = try await planStore()
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

    // MARK: A plan Claude Code holds, carried inside the card

    @MainActor
    private func heldPlanStore(feed: FeedStub = FeedStub()) async throws -> TestStoreOf<CardDetailFeature> {
        let plan = try Fixtures.heldPlanCard(text: planText)
        let store = store(plan, feed: feed, content: { _ in XCTFail("a held plan is not fetched"); return "" })
        await store.send(.screen(.loaded(plan)))
        return store
    }

    @MainActor
    func testAHeldPlansTextIsTheCardsOwnAndCanBeCommented() async throws {
        let store = try await heldPlanStore()

        XCTAssertEqual(store.state.markdown, planText)
        XCTAssertNil(store.state.contentDigest)
        XCTAssertTrue(store.state.canComment)
        XCTAssertFalse(store.state.blocks.filter(\.isCommentable).isEmpty)
    }

    @MainActor
    func testAnAnnotationOnAHeldPlanCanBeAddedThenRemoved() async throws {
        let store = try await heldPlanStore()
        let block = try XCTUnwrap(store.state.blocks.first { $0.plainText.hasPrefix("Build the form") })

        await store.send(.blockTapped(anchor: block.draftKey, quote: block.plainText))
        await store.send(.composingChanged("Which form?"))
        await store.send(.commentSaved)
        XCTAssertEqual(store.state.feedback.map(\.text), ["Which form?"])

        await store.send(.blockTapped(anchor: block.draftKey, quote: block.plainText))
        await store.send(.commentDeleted)
        XCTAssertTrue(store.state.feedback.isEmpty)
        XCTAssertFalse(store.state.hasFeedback)
    }

    @MainActor
    func testRequestingChangesOnAHeldPlanSendsEveryAnnotationWithTheBlockItIsAbout() async throws {
        let feed = FeedStub()
        let store = try await heldPlanStore(feed: feed)
        let blocks = store.state.blocks.filter(\.isCommentable)
        let first = try XCTUnwrap(blocks.first), last = try XCTUnwrap(blocks.last)
        for block in [last, first] {
            await store.send(.blockTapped(anchor: block.draftKey, quote: block.plainText))
            await store.send(.composingChanged("note on \(block.plainText)"))
            await store.send(.commentSaved)
        }
        let changes = try XCTUnwrap(store.state.subject?.buttons.first { $0.id?.hasPrefix("changes:") == true })

        await store.send(.actionTapped(changes))
        await store.skipReceivedActions()

        let comments = try XCTUnwrap(feed.requests.first?.input.comments)
        XCTAssertEqual(comments.map(\.anchor), [first.draftKey, last.draftKey])
        XCTAssertEqual(comments.map(\.quote), [first.plainText, last.plainText])
    }

    // MARK: Sending

    @MainActor
    func testRequestingChangesIsDisabledUntilThereIsFeedbackThenSendsTheCommentsInReadingOrder() async throws {
        let feed = FeedStub()
        let dismissed = LockIsolated(0)
        let store = try await planStore(feed: feed, dismissed: dismissed)
        let changes = try XCTUnwrap(store.state.subject?.buttons.first { $0.id?.hasPrefix("changes:") == true })
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
        await store.send(.replyChanged("Overall: good"))
        XCTAssertTrue(store.state.isSendable(changes))
        await store.send(.actionTapped(changes))
        await store.skipReceivedActions()

        let request = try XCTUnwrap(feed.requests.first)
        XCTAssertEqual(request.actionId, changes.id)
        XCTAssertEqual(request.input.comments?.map(\.anchor), [first.draftKey, last.draftKey])
        XCTAssertEqual(request.input.text, "Overall: good")
        XCTAssertEqual(dismissed.value, 1, "the detail closes as it sends")
        XCTAssertTrue(store.state.comments.isEmpty)
        XCTAssertEqual(store.state.reply, "")
    }

    @MainActor
    func testBlockCommentsAloneSatisfyAChangeRequestThatRequiresText() async throws {
        let feed = FeedStub()
        let store = try await planStore(feed: feed)
        let changes = try XCTUnwrap(store.state.subject?.buttons.first { $0.id?.hasPrefix("changes:") == true })
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
    func testApprovingNeedsNoFeedbackAndClosesTheDetail() async throws {
        let feed = FeedStub()
        let dismissed = LockIsolated(0)
        let store = try await planStore(feed: feed, dismissed: dismissed)
        let approve = try XCTUnwrap(store.state.subject?.buttons.first { $0.id?.hasPrefix("accept:") == true })

        await store.send(.actionTapped(approve))
        await store.skipReceivedActions()
        XCTAssertEqual(feed.requests.map(\.actionId), [approve.id])
        XCTAssertEqual(dismissed.value, 1)
    }

    @MainActor
    func testSendingLeavesAtOnceAndHandsTheRequestToTheAppWithoutWaitingForTheDaemon() async throws {
        let dismissed = LockIsolated(0)
        let performed = LockIsolated(0)
        let plan = try Fixtures.planCard()
        var state = CardDetailFeature.State(key: .card("plan-1"))
        state.screen.document = plan
        state.content = planText
        state.contentDigest = "plan-digest"
        let store = TestStore(initialState: state) { CardDetailFeature() } withDependencies: {
            $0.viewsClient.perform = { _ in
                performed.withValue { $0 += 1 }
                throw CancellationError()
            }
            $0.dismiss = DismissEffect { dismissed.withValue { $0 += 1 } }
        }
        store.exhaustivity = .off
        let block = try XCTUnwrap(store.state.blocks.first { $0.isCommentable })
        await store.send(.blockTapped(anchor: block.draftKey, quote: block.plainText))
        await store.send(.composingChanged("keep me"))
        await store.send(.commentSaved)
        let changes = try XCTUnwrap(store.state.subject?.buttons.first { $0.id?.hasPrefix("changes:") == true })

        await store.send(.actionTapped(changes))
        await store.receive(\.actions.sent)
        await store.receive(\.delegate.handOff)
        await store.finish()
        XCTAssertEqual(dismissed.value, 1)
        XCTAssertEqual(performed.value, 0, "the app delivers it; the detail never waits for the daemon")
        XCTAssertTrue(store.state.comments.isEmpty)
    }

    @MainActor
    func testAnOptionButtonAnswersWithItsOptionID() async throws {
        let feed = FeedStub()
        let store = store(try Fixtures.card(Fixtures.questionID), feed: feed)
        let warm = try XCTUnwrap(store.state.subject?.buttons.last)

        await store.send(.actionTapped(warm))
        await store.skipReceivedActions()
        XCTAssertEqual(feed.requests.first?.optionId, "warm")
        XCTAssertEqual(feed.requests.count, 1)
    }

    @MainActor
    func testTheReplyFieldSendsItsTextToTheNodeAndIsEmptyUntilThereIsSome() async throws {
        let feed = FeedStub()
        let store = store(try Fixtures.card(Fixtures.questionID), feed: feed)
        let reply = try XCTUnwrap(store.state.subject?.replyAction)
        XCTAssertFalse(store.state.isSendable(reply), "an empty reply sends nothing")

        await store.send(.replyChanged("Can you show both?"))
        XCTAssertTrue(store.state.isSendable(reply))
        await store.send(.actionTapped(reply))
        await store.skipReceivedActions()
        XCTAssertEqual(feed.requests.first?.actionId, reply.id)
        XCTAssertEqual(feed.requests.first?.input.text, "Can you show both?")
        XCTAssertNil(feed.requests.first?.optionId)
    }

    @MainActor
    func testReplyingToAnOrchestratorNoticeSendsAtOnceWithoutASheet() async throws {
        let feed = FeedStub()
        let store = store(try Fixtures.orchestratorNotice(), feed: feed)
        let reply = try XCTUnwrap(store.state.subject?.replyAction)

        await store.send(.replyChanged("Restarted it"))
        await store.send(.actionTapped(reply))
        await store.skipReceivedActions()

        XCTAssertNil(store.state.composition, "the reply field is the words: no second field asks for them")
        XCTAssertEqual(feed.requests.map(\.actionId), ["msg:view@7"])
        XCTAssertEqual(feed.requests.first?.input.text, "Restarted it")
    }

    @MainActor
    func testRequestingClarificationFromTheDetailAsksForTheReasonFirst() async throws {
        let feed = FeedStub()
        let store = store(try Fixtures.card(Fixtures.questionID), feed: feed)
        let clarify = try XCTUnwrap(store.state.subject?.menu.first { $0.id?.hasPrefix("clarify:") == true })

        await store.send(.actionTapped(clarify))
        XCTAssertEqual(store.state.composition?.action, clarify)
        XCTAssertEqual(feed.requests, [])
        await store.send(.compositionConfirmed)
        XCTAssertEqual(feed.requests, [], "no reason, nothing sent")
        await store.send(.reasonChanged("unclear"))
        await store.send(.noteChanged("Which copy ships first?"))
        await store.send(.compositionConfirmed)
        await store.skipReceivedActions()

        let request = try XCTUnwrap(feed.requests.first)
        XCTAssertEqual(request.actionId, clarify.id)
        XCTAssertEqual(request.optionId, "unclear")
        XCTAssertEqual(request.input.text, "Which copy ships first?")
        XCTAssertNil(store.state.composition)
    }

    @MainActor
    func testALinkActionOpensItsScreenInsteadOfSending() async throws {
        let feed = FeedStub()
        let store = store(try Fixtures.card(Fixtures.questionID), feed: feed)
        let action = try ContractCodec.decode(AlondraContract.Action.self, from: Data(
            #"{"label":"Open","style":"secondary","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":true,"link":{"kind":"view","key":"task:t-1"}}"#.utf8))

        await store.send(.actionTapped(action))
        await store.receive(\.delegate.open, ViewKey(rawValue: "task:t-1"))
        XCTAssertEqual(feed.requests, [])
    }

    @MainActor
    func testAnActionTheDaemonDisabledSendsNothing() async throws {
        let feed = FeedStub()
        let store = store(try Fixtures.card(Fixtures.pullRequestID), feed: feed)
        let merge = try ContractCodec.decode(AlondraContract.Action.self, from: Data(
            #"{"id":"merge:github.com/example/repo#12@1","label":"Merge","style":"primary","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":false,"disabled_reason":"Answer the question first"}"#.utf8))
        XCTAssertFalse(merge.enabled)

        await store.send(.actionTapped(merge))
        XCTAssertEqual(feed.requests, [])
        XCTAssertNil(store.state.actions.action)
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
        XCTAssertNil(store.state.subject?.card)
        XCTAssertEqual(store.state.subject?.title, "Release plan")
        XCTAssertEqual(labels(store), ["Approve", "Request changes"])
        XCTAssertNil(store.state.subject?.replyAction)
        XCTAssertTrue(store.state.canComment)
    }

    @MainActor
    func testTheScreenObservesItsOwnKeyAndStopsWhenAsked() async throws {
        let card = try Fixtures.card(Fixtures.questionID)
        let feed = FeedStub([card])
        let state = CardDetailFeature.State(key: ViewKey(rawValue: card.key))
        let store = TestStore(initialState: state) { CardDetailFeature() } withDependencies: {
            $0.viewsClient.observe = feed.observe
        }
        store.exhaustivity = .off

        await store.send(.task)
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.subject?.title, "Choose the welcome copy")
        XCTAssertEqual(store.state.actions.dataEpoch, "test-epoch")
    }
}
