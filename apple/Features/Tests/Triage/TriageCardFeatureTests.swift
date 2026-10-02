import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import Foundation
import XCTest

final class TriageCardFeatureTests: XCTestCase {
    @MainActor
    private func store(
        _ id: String, feed: FeedStub = FeedStub(), loaded document: ViewDocument? = nil
    ) async throws -> TestStoreOf<TriageCardFeature> {
        var state = TriageCardFeature.State(id: id)
        if let document {
            state.screen.document = document
            state.screen.phase = .loaded
            state.actions.dataEpoch = document.dataEpoch
        }
        let store = TestStore(initialState: state) { TriageCardFeature() } withDependencies: {
            $0.viewsClient.observe = feed.observe
            $0.viewsClient.perform = feed.perform()
        }
        store.exhaustivity = .off
        return store
    }

    private func action(
        _ store: TestStoreOf<TriageCardFeature>, verb: String
    ) throws -> AlondraContract.Action {
        try XCTUnwrap(store.state.allActions.first { $0.id?.hasPrefix(verb + ":") == true }, verb)
    }

    // MARK: Loading

    @MainActor
    func testTheCardObservesItsOwnViewAndKeepsTheServersEpochForItsActions() async throws {
        let store = try await store(Fixtures.questionID, feed: FeedStub([try Fixtures.card(Fixtures.questionID)]))
        await store.send(.screen(.task))
        await store.skipReceivedActions()

        XCTAssertEqual(store.state.card?.title, "Restore the native review experience")
        XCTAssertEqual(store.state.actions.dataEpoch, "test-epoch")
    }

    // MARK: Options and the reply field

    @MainActor
    func testTappingAnOptionThatNeedsNoTextAnswersWithItAtOnce() async throws {
        let feed = FeedStub()
        let store = try await store("plan-r1", feed: feed, loaded: try Fixtures.planQuestionCard())
        let answer = try XCTUnwrap(store.state.replyAction)
        XCTAssertEqual(answer.presentation, .options)
        let option = try XCTUnwrap(answer.options.first)

        await store.send(.optionPicked(answer, optionID: option.id))
        await store.skipReceivedActions()
        let request = try XCTUnwrap(feed.requests.first, "tapping the option is the answer")
        XCTAssertEqual(feed.requests.count, 1)
        XCTAssertEqual(request.actionId, answer.id)
        XCTAssertEqual(request.optionId, option.id)
        XCTAssertNil(request.input.text)
        XCTAssertEqual(request.dataEpoch, "test-epoch")
        XCTAssertEqual(store.state.stamp, "Answered", "the stamp shows once the server applied it")
    }

    @MainActor
    func testAnOptionThatNeedsTextIsPickedAndTheReplyFieldSendsItWithTheText() async throws {
        let feed = FeedStub()
        let store = try await store("hold-1", feed: feed, loaded: try Fixtures.heldPlanCard())
        let answer = try XCTUnwrap(store.state.replyAction)

        await store.send(.optionPicked(answer, optionID: "request_changes"))
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.actions.selectedOptionID, "request_changes")
        XCTAssertEqual(feed.requests, [], "an option that asks for text waits for the reply field")

        await store.send(.replyChanged("Split the lexer first"))
        await store.send(.replySent(answer))
        await store.skipReceivedActions()
        let request = try XCTUnwrap(feed.requests.first)
        XCTAssertEqual(request.optionId, "request_changes")
        XCTAssertEqual(request.input.text, "Split the lexer first")
    }

    @MainActor
    private func flakyStore(
        _ id: String, loaded document: ViewDocument
    ) -> (TestStoreOf<TriageCardFeature>, LockIsolated<[ActionRequest]>, LockIsolated<Bool>) {
        let attempts = LockIsolated<[ActionRequest]>([])
        let failing = LockIsolated(true)
        var state = TriageCardFeature.State(id: id)
        state.screen.document = document
        state.screen.phase = .loaded
        state.actions.dataEpoch = document.dataEpoch
        let store = TestStore(initialState: state) { TriageCardFeature() } withDependencies: {
            $0.viewsClient.perform = { request in
                attempts.withValue { $0.append(request) }
                if failing.value { throw URLError(.timedOut) }
                return try ContractCodec.decode(ActionResponse.self, from: Data(
                    #"{"protocol":1,"outcome":"applied","message":"Done","stamp":"Answered"}"#.utf8))
            }
            $0.uuid = .incrementing
        }
        store.exhaustivity = .off
        return (store, attempts, failing)
    }

    @MainActor
    func testRetryingTheSameOptionAfterAFailureKeepsItsCommandAndAnotherOptionIsANewCommand() async throws {
        let (store, attempts, failing) = flakyStore("perm-1", loaded: try Fixtures.permissionCard())
        let answer = try XCTUnwrap(store.state.replyAction)

        await store.send(.optionPicked(answer, optionID: "allow"))
        await store.skipReceivedActions()
        XCTAssertFalse(store.state.actions.inFlight)
        XCTAssertEqual(store.state.actions.error, "Couldn’t send. Try again.")

        await store.send(.optionPicked(answer, optionID: "allow"))
        await store.skipReceivedActions()
        failing.setValue(false)
        await store.send(.optionPicked(answer, optionID: "deny"))
        await store.skipReceivedActions()

        let sent = attempts.value
        XCTAssertEqual(sent.map(\.optionId), ["allow", "allow", "deny"])
        guard sent.count == 3 else { return }
        XCTAssertEqual(sent[0].commandId, sent[1].commandId, "a retry is the same command, which the daemon deduplicates")
        XCTAssertNotEqual(sent[1].commandId, sent[2].commandId, "a different answer is a different command")
        XCTAssertEqual(store.state.stamp, "Answered")
    }

    @MainActor
    func testEditingTheReplyAfterAFailureSendsTheNewTextAsANewCommand() async throws {
        let (store, attempts, failing) = flakyStore(Fixtures.questionID, loaded: try Fixtures.card(Fixtures.questionID))
        let answer = try XCTUnwrap(store.state.replyAction)

        await store.send(.replyChanged("Ship the warm one"))
        await store.send(.replySent(answer))
        await store.skipReceivedActions()
        failing.setValue(false)
        await store.send(.replyChanged("Ship the clear one"))
        await store.send(.replySent(answer))
        await store.skipReceivedActions()

        let sent = attempts.value
        XCTAssertEqual(sent.map(\.input.text), ["Ship the warm one", "Ship the clear one"])
        guard sent.count == 2 else { return }
        XCTAssertNotEqual(sent[0].commandId, sent[1].commandId)
    }

    @MainActor
    func testTheStampOnlyShowsAfterTheServerAnswers() async throws {
        let feed = FeedStub()
        let (gate, release) = AsyncStream<Void>.makeStream()
        let card = try Fixtures.card(Fixtures.questionID)
        var state = TriageCardFeature.State(id: Fixtures.questionID)
        state.screen.document = card
        let store = TestStore(initialState: state) { TriageCardFeature() } withDependencies: {
            $0.viewsClient.perform = { request in
                for await _ in gate { break }
                return try await feed.perform()(request)
            }
        }
        store.exhaustivity = .off
        let answer = try XCTUnwrap(store.state.replyAction)
        let option = try XCTUnwrap(answer.options.first { if case .none = $0.input { true } else { false } })

        await store.send(.optionPicked(answer, optionID: option.id))
        await store.receive(\.actions.submit)
        XCTAssertTrue(store.state.actions.inFlight)
        XCTAssertNil(store.state.stamp, "sent is not applied")
        release.yield()
        await store.receive(\.actions.response)
        XCTAssertEqual(store.state.stamp, "Answered")
    }

    @MainActor
    func testTypingAfterAFailedOptionThatNeedsNoTextDropsThePick() async throws {
        let card = try Fixtures.card(Fixtures.questionID)
        var state = TriageCardFeature.State(id: Fixtures.questionID)
        state.screen.document = card
        let store = TestStore(initialState: state) { TriageCardFeature() } withDependencies: {
            $0.viewsClient.perform = { _ in throw URLError(.notConnectedToInternet) }
        }
        store.exhaustivity = .off
        let answer = try XCTUnwrap(store.state.replyAction)
        let plain = try XCTUnwrap(answer.options.first { if case .none = $0.input { true } else { false } })

        await store.send(.optionPicked(answer, optionID: plain.id))
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.actions.selectedOptionID, plain.id)
        await store.send(.replyChanged("Actually, neither"))
        await store.skipReceivedActions()
        XCTAssertNil(store.state.actions.selectedOptionID)
        XCTAssertEqual(store.state.replyDraft, "Actually, neither")
    }

    @MainActor
    func testASentFreeTextReplyCarriesTheDraftAndTheDraftIsClearedOnceApplied() async throws {
        let feed = FeedStub()
        let store = try await store(Fixtures.questionID, feed: feed, loaded: try Fixtures.card(Fixtures.questionID))
        let answer = try XCTUnwrap(store.state.replyAction)

        await store.send(.replyChanged("Ship the warmer one"))
        XCTAssertEqual(store.state.replyDraft, "Ship the warmer one")
        await store.send(.replySent(answer))
        await store.skipReceivedActions()

        let request = try XCTUnwrap(feed.requests.first)
        XCTAssertEqual(request.input.text, "Ship the warmer one")
        XCTAssertEqual(store.state.replyDraft, "")
    }

    @MainActor
    func testAFailedSendKeepsTheDraftAndReportsUpward() async throws {
        struct Offline: Error {}
        var state = TriageCardFeature.State(id: Fixtures.questionID)
        state.screen.document = try Fixtures.card(Fixtures.questionID)
        let store = TestStore(initialState: state) { TriageCardFeature() } withDependencies: {
            $0.viewsClient.perform = { _ in throw Offline() }
        }
        store.exhaustivity = .off
        let answer = try XCTUnwrap(store.state.replyAction)

        await store.send(.replyChanged("Ship the warmer one"))
        await store.send(.replySent(answer))
        await store.receive(\.actions.failed)
        await store.receive(\.actions.delegate.failed)
        XCTAssertEqual(store.state.replyDraft, "Ship the warmer one")
        XCTAssertNil(store.state.stamp)
        XCTAssertFalse(store.state.actions.inFlight, "the card can be answered again")
    }

    // MARK: I need more context

    @MainActor
    func testAskingForContextCollectsAnOptionalNoteThenSendsIt() async throws {
        let feed = FeedStub()
        let store = try await store(Fixtures.questionID, feed: feed, loaded: try Fixtures.card(Fixtures.questionID))
        let context = try action(store, verb: "ctx")

        await store.send(.actionTapped(context))
        XCTAssertEqual(store.state.contextRequest?.action, context)
        XCTAssertEqual(feed.requests, [], "the alert comes first")
        await store.send(.contextNoteChanged("What does the old copy say?"))
        await store.send(.contextConfirmed)
        await store.skipReceivedActions()

        let request = try XCTUnwrap(feed.requests.first)
        XCTAssertEqual(request.actionId, context.id)
        XCTAssertEqual(request.input.text, "What does the old copy say?")
        XCTAssertNil(store.state.contextRequest)
    }

    @MainActor
    func testCancellingTheContextAlertSendsNothing() async throws {
        let feed = FeedStub()
        let store = try await store(Fixtures.questionID, feed: feed, loaded: try Fixtures.card(Fixtures.questionID))
        await store.send(.actionTapped(try action(store, verb: "ctx")))
        await store.send(.contextCancelled)
        XCTAssertNil(store.state.contextRequest)
        XCTAssertEqual(feed.requests, [])
    }

    // MARK: Reports and deliveries

    @MainActor
    func testRescuingCommentsOpensTheDeliveryInsteadOfSending() async throws {
        let feed = FeedStub()
        let store = try await store(Fixtures.deliveryID, feed: feed, loaded: try Fixtures.liveDelivery())
        let rescue = try action(store, verb: "changes")
        XCTAssertEqual(rescue.comments?.kind, .blocks)
        XCTAssertEqual(try action(store, verb: "reply").id, store.state.replyAction?.id)

        await store.send(.actionTapped(rescue))
        await store.receive(\.opened, .card(Fixtures.deliveryID))
        await store.receive(\.delegate.open, .card(Fixtures.deliveryID))
        XCTAssertEqual(feed.requests, [])
    }

    @MainActor
    func testADeliveryWithoutADocumentRepliesToTheNodeAndNeverRescuesComments() async throws {
        let feed = FeedStub()
        let store = try await store(Fixtures.deliveryID, feed: feed, loaded: try Fixtures.livePlainDelivery())
        let reply = try XCTUnwrap(store.state.replyAction)
        XCTAssertEqual(reply.id, try action(store, verb: "reply").id)
        XCTAssertNil(reply.comments)
        XCTAssertNil(store.state.allActions.first { $0.id?.hasPrefix("changes:") == true })
        guard case let .text(placeholder, _, _) = reply.input else { return XCTFail("reply input is text") }
        XCTAssertEqual(placeholder, "Reply to this node…")
        let card = try XCTUnwrap(store.state.card)
        XCTAssertFalse(CardActionPresentation.isRescueComments(card: card, action: reply))

        await store.send(.replyChanged("Looks good, one more pass"))
        await store.send(.replySent(reply))
        await store.skipReceivedActions()
        XCTAssertEqual(feed.requests.first?.actionId, reply.id)
        XCTAssertEqual(feed.requests.first?.input.text, "Looks good, one more pass")
        XCTAssertEqual(feed.requests.count, 1)
    }

    @MainActor
    func testAcceptingADeliverySendsStraightAway() async throws {
        let feed = FeedStub()
        let store = try await store(Fixtures.deliveryID, feed: feed, loaded: try Fixtures.liveDelivery())
        let accept = try action(store, verb: "accept")

        await store.send(.actionTapped(accept))
        await store.skipReceivedActions()
        XCTAssertEqual(feed.requests.map(\.actionId), [accept.id])
    }

    @MainActor
    func testMarkingAReportAsReadSendsStraightAway() async throws {
        let feed = FeedStub()
        let store = try await store(Fixtures.informationID, feed: feed, loaded: try Fixtures.card(Fixtures.informationID))
        let read = try action(store, verb: "read")

        await store.send(.actionTapped(read))
        await store.skipReceivedActions()
        XCTAssertEqual(feed.requests.map(\.actionId), [read.id])
    }

    // MARK: Pull requests

    @MainActor
    func testAskingAboutAPullRequestSendsTheDraftAsTheQuestion() async throws {
        let feed = FeedStub()
        let store = try await store(Fixtures.pullRequestID, feed: feed, loaded: try Fixtures.card(Fixtures.pullRequestID))
        let ask = try action(store, verb: "ask_pr")

        await store.send(.replyChanged("Why a new cache?"))
        await store.send(.actionTapped(ask))
        await store.skipReceivedActions()
        let request = try XCTUnwrap(feed.requests.first)
        XCTAssertEqual(request.actionId, ask.id)
        XCTAssertEqual(request.input.text, "Why a new cache?")
    }

    @MainActor
    func testALinkOnTheCardOpensItsScreen() async throws {
        let store = try await store(Fixtures.questionID, loaded: try Fixtures.card(Fixtures.questionID))
        await store.send(.opened(.node("n-1")))
        await store.receive(\.delegate.open, .node("n-1"))
    }

    // MARK: Permission and plan cards

    @MainActor
    func testAPermissionCardOffersAllowAndDenyAsTheOptionsToPickFrom() async throws {
        let feed = FeedStub()
        let permission = try Fixtures.permissionCard()
        let store = try await store("perm-1", feed: feed, loaded: permission)
        let answer = try XCTUnwrap(store.state.replyAction)
        XCTAssertEqual(answer.options.map(\.label), ["Allow", "Deny"])
        XCTAssertEqual(store.state.card?.icon, .question)
        XCTAssertTrue(store.state.card?.body?.text.hasPrefix("```") == true, "the prompt arrives as a code block")

        await store.send(.optionPicked(answer, optionID: "deny"))
        await store.skipReceivedActions()
        let request = try XCTUnwrap(feed.requests.first)
        XCTAssertEqual(request.optionId, "deny")
    }

    @MainActor
    func testAPlanCardApprovesOrOpensTheDocumentToReview() async throws {
        let feed = FeedStub()
        let plan = try Fixtures.planCard()
        let store = try await store("plan-1", feed: feed, loaded: plan)
        XCTAssertEqual(store.state.card?.icon, .document)

        await store.send(.actionTapped(try action(store, verb: "changes")))
        await store.receive(\.delegate.open, .card("plan-1"))
        await store.send(.actionTapped(try action(store, verb: "accept")))
        await store.skipReceivedActions()
        XCTAssertEqual(feed.requests.map(\.actionId), [try action(store, verb: "accept").id])
    }
}
