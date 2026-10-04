import AlondraContract
import AlondraDesign
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
        _ store: TestStoreOf<TriageCardFeature>, verb: String, label: String? = nil
    ) throws -> AlondraContract.Action {
        try XCTUnwrap(store.state.allActions.first {
            $0.id?.hasPrefix(verb + ":") == true && (label == nil || $0.label == label)
        }, verb)
    }

    // MARK: Loading

    @MainActor
    func testTheCardObservesItsOwnViewAndKeepsTheServersEpochForItsActions() async throws {
        let store = try await store(Fixtures.questionID, feed: FeedStub([try Fixtures.card(Fixtures.questionID)]))
        await store.send(.screen(.task))
        await store.skipReceivedActions()

        XCTAssertEqual(store.state.card?.need, "Choose the welcome copy")
        XCTAssertEqual(store.state.actions.dataEpoch, "test-epoch")
    }

    // MARK: The buttons

    @MainActor
    func testTheButtonsAreTheDaemonsActionsInItsOrderAndMoreHoldsTheExtras() async throws {
        let store = try await store(Fixtures.questionID, loaded: try Fixtures.card(Fixtures.questionID))

        XCTAssertEqual(store.state.buttons.map(\.label), ["★ Ship the concise copy", "Keep the warmer copy"])
        XCTAssertEqual(store.state.buttons.map(\.role), [.advance, .neutral])
        XCTAssertEqual(store.state.menu.map(\.label), ["Write my own answer", "Request clarification"])
        XCTAssertEqual(store.state.replyAction?.id, "reply:\(Fixtures.questionID)@1")
    }

    @MainActor
    func testAtMostThreeButtonsShowAndTheRestGoUnderMoreBeforeTheExtras() async throws {
        let store = try await store("decide-5", loaded: try Fixtures.crowdedDecision())

        XCTAssertEqual(store.state.buttons.count, 3)
        XCTAssertEqual(store.state.buttons.map(\.label), ["★ Stable", "Beta", "Nightly"])
        XCTAssertEqual(
            store.state.menu.map(\.label), ["Preview", "Internal", "Write my own answer", "Request clarification"]
        )
        XCTAssertEqual(store.state.allActions.count, 8, "three buttons, four in More, and the reply field")
    }

    @MainActor
    func testACardWithTwoButtonsShowsTwoAndAnEmptyMoreMenuWhenThereIsNothingElse() async throws {
        let store = try await store(Fixtures.informationID, loaded: try Fixtures.card(Fixtures.informationID))
        XCTAssertEqual(store.state.buttons.map(\.label), ["Seen"])
        XCTAssertTrue(store.state.menu.isEmpty)
    }

    // MARK: One tap answers

    @MainActor
    func testTappingAnOptionButtonAnswersWithItsOptionIDAtOnce() async throws {
        let feed = FeedStub()
        let store = try await store(Fixtures.questionID, feed: feed, loaded: try Fixtures.card(Fixtures.questionID))
        let warm = store.state.buttons[1]

        await store.send(.actionTapped(warm))
        await store.skipReceivedActions()
        let request = try XCTUnwrap(feed.requests.first, "tapping the button is the answer")
        XCTAssertEqual(feed.requests.count, 1)
        XCTAssertEqual(request.actionId, "ans:\(Fixtures.questionID)@1")
        XCTAssertEqual(request.optionId, "warm", "the button's own option_id travels with the answer")
        XCTAssertNil(request.input.text)
        XCTAssertEqual(request.dataEpoch, "test-epoch")
        XCTAssertEqual(store.state.stamp, "Answered", "the stamp shows once the server applied it")
    }

    @MainActor
    func testEachOptionButtonOfTheSameQuestionSendsItsOwnOption() async throws {
        let feed = FeedStub()
        let store = try await store("perm-1", feed: feed, loaded: try Fixtures.authorizeCard())

        for button in store.state.buttons {
            await store.send(.actionTapped(button))
            await store.skipReceivedActions()
        }
        XCTAssertEqual(feed.requests.map(\.optionId), ["allow", "always", "deny"])
    }

    @MainActor
    func testRetryingTheSameButtonAfterAFailureKeepsItsCommandAndAnotherButtonIsANewCommand() async throws {
        let attempts = LockIsolated<[ActionRequest]>([])
        let failing = LockIsolated(true)
        let card = try Fixtures.authorizeCard()
        var state = TriageCardFeature.State(id: "perm-1")
        state.screen.document = card
        state.screen.phase = .loaded
        state.actions.dataEpoch = card.dataEpoch
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
        let yes = store.state.buttons[0]
        let no = store.state.buttons[2]

        await store.send(.actionTapped(yes))
        await store.skipReceivedActions()
        XCTAssertFalse(store.state.actions.inFlight)
        XCTAssertEqual(store.state.actions.error, "Couldn’t send. Try again.")
        await store.send(.actionTapped(yes))
        await store.skipReceivedActions()
        failing.setValue(false)
        await store.send(.actionTapped(no))
        await store.skipReceivedActions()

        let sent = attempts.value
        XCTAssertEqual(sent.map(\.optionId), ["allow", "allow", "deny"])
        guard sent.count == 3 else { return }
        XCTAssertEqual(sent[0].commandId, sent[1].commandId, "a retry is the same command, which the daemon deduplicates")
        XCTAssertNotEqual(sent[1].commandId, sent[2].commandId, "a different answer is a different command")
        XCTAssertEqual(store.state.stamp, "Answered")
    }

    @MainActor
    func testTheStampShowsTheMomentTheAnswerIsSentWithoutWaitingForTheServer() async throws {
        let feed = FeedStub()
        let (gate, release) = AsyncStream<Void>.makeStream()
        var state = TriageCardFeature.State(id: Fixtures.questionID)
        state.screen.document = try Fixtures.card(Fixtures.questionID)
        let store = TestStore(initialState: state) { TriageCardFeature() } withDependencies: {
            $0.viewsClient.perform = { request in
                for await _ in gate { break }
                return try await feed.perform()(request)
            }
        }
        store.exhaustivity = .off

        await store.send(.actionTapped(store.state.buttons[0]))
        await store.receive(\.submit)
        await store.receive(\.actions.sent)
        XCTAssertTrue(store.state.actions.inFlight)
        XCTAssertEqual(store.state.stamp, "Answered", "the stamp does not wait for the daemon")
        release.yield()
        await store.receive(\.actions.settled)
        await store.receive(\.actions.response)
        XCTAssertEqual(store.state.stamp, "Answered")
    }

    @MainActor
    func testADisabledButtonSendsNothing() async throws {
        let feed = FeedStub()
        let store = try await store(Fixtures.pullRequestID, feed: feed, loaded: try Fixtures.card(Fixtures.pullRequestID))
        let merge = try action(store, verb: "merge")
        XCTAssertFalse(merge.enabled)
        XCTAssertNotNil(merge.disabledReason, "the card says why")

        await store.send(.actionTapped(merge))
        XCTAssertNil(store.state.actions.action)
        XCTAssertEqual(feed.requests, [])
    }

    @MainActor
    func testMarkingAReportAsSeenSendsStraightAway() async throws {
        let feed = FeedStub()
        let store = try await store(Fixtures.informationID, feed: feed, loaded: try Fixtures.card(Fixtures.informationID))
        let seen = try action(store, verb: "read")

        await store.send(.actionTapped(seen))
        await store.skipReceivedActions()
        XCTAssertEqual(feed.requests.map(\.actionId), [seen.id])
    }

    // MARK: Merging

    @MainActor
    func testMergingAsksForConfirmationAndSendsTheChosenMethod() async throws {
        let feed = FeedStub()
        let pullRequest = try Fixtures.unblocked(try Fixtures.card(Fixtures.pullRequestID))
        let store = try await store(Fixtures.pullRequestID, feed: feed, loaded: pullRequest)
        let merge = try action(store, verb: "merge")
        XCTAssertEqual(merge.presentation, .menu)

        await store.send(.menuOptionPicked(merge, optionID: "squash"))
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.actions.confirmation?.title, "Merge PR #12?")
        XCTAssertEqual(feed.requests, [], "nothing goes before the confirmation")
        await store.send(.actions(.confirm))
        await store.skipReceivedActions()
        XCTAssertEqual(feed.requests.map(\.optionId), ["squash"])
    }

    @MainActor
    func testRequestingChangesOnAPullRequestOpensItsDiffToComment() async throws {
        let feed = FeedStub()
        let store = try await store(Fixtures.pullRequestID, feed: feed, loaded: try Fixtures.card(Fixtures.pullRequestID))
        let changes = try action(store, verb: "changes")
        XCTAssertEqual(changes.comments?.kind, .diffLines)

        await store.send(.actionTapped(changes))
        await store.receive(\.opened, .pullRequest("github.com/example/repo#12"))
        await store.receive(\.delegate.open, .pullRequest("github.com/example/repo#12"))
        XCTAssertEqual(feed.requests, [])
    }

    @MainActor
    func testOpenOnGitHubIsLeftToTheViewToOpen() async throws {
        let feed = FeedStub()
        let store = try await store(Fixtures.pullRequestID, feed: feed, loaded: try Fixtures.card(Fixtures.pullRequestID))
        let github = try XCTUnwrap(store.state.menu.first { $0.label == "Open on GitHub" })

        await store.send(.actionTapped(github))
        XCTAssertNil(store.state.actions.action)
        XCTAssertEqual(feed.requests, [])
    }

    // MARK: Writing to the node

    @MainActor
    func testASentReplyCarriesTheDraftAndTheDraftIsClearedOnceApplied() async throws {
        let feed = FeedStub()
        let store = try await store(Fixtures.questionID, feed: feed, loaded: try Fixtures.card(Fixtures.questionID))
        let reply = try XCTUnwrap(store.state.replyAction)

        await store.send(.replyChanged("Ship the warmer one"))
        XCTAssertEqual(store.state.replyDraft, "Ship the warmer one")
        await store.send(.replySent(reply))
        await store.skipReceivedActions()

        let request = try XCTUnwrap(feed.requests.first)
        XCTAssertEqual(request.actionId, reply.id)
        XCTAssertNil(request.optionId, "a reply answers no option")
        XCTAssertEqual(request.input.text, "Ship the warmer one")
        XCTAssertEqual(store.state.replyDraft, "")
    }

    @MainActor
    func testEveryKindOfCardHasTheReplyField() async throws {
        for document in [
            try Fixtures.card(Fixtures.questionID), try Fixtures.card(Fixtures.informationID),
            try Fixtures.card(Fixtures.pullRequestID), try Fixtures.authorizeCard(), try Fixtures.problemCard(),
            try Fixtures.planCard(),
        ] {
            let store = try await store("x", loaded: document)
            XCTAssertEqual(store.state.replyAction?.id?.hasPrefix("reply:"), true, document.key)
        }
    }

    @MainActor
    func testAFailedReplyKeepsTheDraftAndReportsUpward() async throws {
        struct Offline: Error {}
        var state = TriageCardFeature.State(id: Fixtures.questionID)
        state.screen.document = try Fixtures.card(Fixtures.questionID)
        let store = TestStore(initialState: state) { TriageCardFeature() } withDependencies: {
            $0.viewsClient.perform = { _ in throw Offline() }
        }
        store.exhaustivity = .off
        let reply = try XCTUnwrap(store.state.replyAction)

        await store.send(.replyChanged("Ship the warmer one"))
        await store.send(.replySent(reply))
        await store.receive(\.actions.failed)
        await store.receive(\.actions.delegate.failed)
        XCTAssertEqual(store.state.replyDraft, "Ship the warmer one")
        XCTAssertNil(store.state.stamp)
        XCTAssertFalse(store.state.actions.inFlight, "the card can be answered again")
    }

    @MainActor
    func testEditingTheReplyAfterAFailureSendsTheNewTextAsANewCommand() async throws {
        let attempts = LockIsolated<[ActionRequest]>([])
        let failing = LockIsolated(true)
        var state = TriageCardFeature.State(id: Fixtures.questionID)
        state.screen.document = try Fixtures.card(Fixtures.questionID)
        let store = TestStore(initialState: state) { TriageCardFeature() } withDependencies: {
            $0.viewsClient.perform = { request in
                attempts.withValue { $0.append(request) }
                if failing.value { throw URLError(.timedOut) }
                return try ContractCodec.decode(ActionResponse.self, from: Data(
                    #"{"protocol":1,"outcome":"applied","message":"Done","stamp":"Sent"}"#.utf8))
            }
            $0.uuid = .incrementing
        }
        store.exhaustivity = .off
        let reply = try XCTUnwrap(store.state.replyAction)

        await store.send(.replyChanged("Ship the warm one"))
        await store.send(.replySent(reply))
        await store.skipReceivedActions()
        failing.setValue(false)
        await store.send(.replyChanged("Ship the clear one"))
        await store.send(.replySent(reply))
        await store.skipReceivedActions()

        let sent = attempts.value
        XCTAssertEqual(sent.map(\.input.text), ["Ship the warm one", "Ship the clear one"])
        guard sent.count == 2 else { return }
        XCTAssertNotEqual(sent[0].commandId, sent[1].commandId)
    }

    @MainActor
    func testWritingMyOwnAnswerOpensTheSheetAndSendsTheWords() async throws {
        let feed = FeedStub()
        let store = try await store(Fixtures.questionID, feed: feed, loaded: try Fixtures.card(Fixtures.questionID))
        let own = try action(store, verb: "ans", label: "Write my own answer")

        await store.send(.actionTapped(own))
        XCTAssertEqual(store.state.composition?.action, own)
        XCTAssertEqual(store.state.composition?.canSend, false, "the words are required")
        XCTAssertEqual(feed.requests, [], "the sheet comes first")
        await store.send(.noteChanged("Neither: keep the old copy"))
        XCTAssertEqual(store.state.composition?.canSend, true)
        await store.send(.compositionConfirmed)
        await store.skipReceivedActions()

        let request = try XCTUnwrap(feed.requests.first)
        XCTAssertEqual(request.actionId, own.id)
        XCTAssertEqual(request.input.text, "Neither: keep the old copy")
        XCTAssertNil(store.state.composition)
    }

    // MARK: Request clarification

    @MainActor
    func testRequestingClarificationAsksForTheReasonThenSendsItWithTheNote() async throws {
        let feed = FeedStub()
        let store = try await store(Fixtures.questionID, feed: feed, loaded: try Fixtures.card(Fixtures.questionID))
        let clarify = try action(store, verb: "clarify")
        XCTAssertEqual(clarify.options.map(\.id), ["unclear", "contradicts", "missing_context"])

        await store.send(.actionTapped(clarify))
        XCTAssertEqual(store.state.composition?.needsReason, true)
        XCTAssertEqual(store.state.composition?.canSend, false, "a reason has to be picked first")
        await store.send(.compositionConfirmed)
        XCTAssertEqual(feed.requests, [], "no reason, nothing sent")

        await store.send(.reasonChanged("contradicts"))
        await store.send(.noteChanged("The two options say opposite things"))
        XCTAssertEqual(store.state.composition?.canSend, true)
        await store.send(.compositionConfirmed)
        await store.skipReceivedActions()

        let request = try XCTUnwrap(feed.requests.first)
        XCTAssertEqual(request.actionId, clarify.id)
        XCTAssertEqual(request.optionId, "contradicts")
        XCTAssertEqual(request.input.text, "The two options say opposite things")
    }

    @MainActor
    func testTheClarificationNoteIsOptional() async throws {
        let feed = FeedStub()
        let store = try await store(Fixtures.questionID, feed: feed, loaded: try Fixtures.card(Fixtures.questionID))

        await store.send(.actionTapped(try action(store, verb: "clarify")))
        await store.send(.reasonChanged("missing_context"))
        await store.send(.compositionConfirmed)
        await store.skipReceivedActions()

        let request = try XCTUnwrap(feed.requests.first)
        XCTAssertEqual(request.optionId, "missing_context")
        XCTAssertNil(request.input.text)
    }

    @MainActor
    func testCancellingTheSheetSendsNothing() async throws {
        let feed = FeedStub()
        let store = try await store(Fixtures.questionID, feed: feed, loaded: try Fixtures.card(Fixtures.questionID))
        await store.send(.actionTapped(try action(store, verb: "clarify")))
        await store.send(.compositionCancelled)
        XCTAssertNil(store.state.composition)
        XCTAssertEqual(feed.requests, [])
    }

    // MARK: Reviewing

    @MainActor
    func testApprovingAPlanSendsAndRequestingChangesOpensItsDetailToComment() async throws {
        let feed = FeedStub()
        let store = try await store("plan-1", feed: feed, loaded: try Fixtures.planCard())
        XCTAssertEqual(store.state.card?.kind, .review)

        await store.send(.actionTapped(try action(store, verb: "changes")))
        await store.receive(\.delegate.open, .card("plan-1"))
        XCTAssertEqual(feed.requests, [])
        await store.send(.actionTapped(try action(store, verb: "accept")))
        await store.skipReceivedActions()
        XCTAssertEqual(feed.requests.map(\.actionId), [try action(store, verb: "accept").id])
    }

    @MainActor
    func testALinkOnTheCardOpensItsScreen() async throws {
        let store = try await store(Fixtures.questionID, loaded: try Fixtures.card(Fixtures.questionID))
        await store.send(.opened(.node("n-1")))
        await store.receive(\.delegate.open, .node("n-1"))
    }

    // MARK: The swipe

    @MainActor
    func testSwipingRightAnswersWithTheFirstButtonAndLeftWithTheLast() async throws {
        let store = try await store("perm-1", loaded: try Fixtures.authorizeCard())
        XCTAssertEqual(store.state.swipeButton(.right)?.label, "Yes")
        XCTAssertEqual(store.state.swipeButton(.left)?.label, "No")

        let crowded = try await self.store("decide-5", loaded: try Fixtures.crowdedDecision())
        XCTAssertEqual(crowded.state.swipeButton(.right)?.label, "★ Stable")
        XCTAssertEqual(crowded.state.swipeButton(.left)?.label, "Nightly", "the last of the three showing, not of More")

        let report = try await self.store(Fixtures.informationID, loaded: try Fixtures.card(Fixtures.informationID))
        XCTAssertEqual(report.state.swipeButton(.right)?.label, "Seen")
        XCTAssertEqual(report.state.swipeButton(.left)?.label, "Seen")
    }

    @MainActor
    func testOnlyAButtonOneTapAnswersCanBeSwiped() async throws {
        let pullRequest = try await store(Fixtures.pullRequestID, loaded: try Fixtures.card(Fixtures.pullRequestID))
        XCTAssertFalse(pullRequest.state.buttons[0].answersOnATap, "a disabled merge menu with a confirmation")
        XCTAssertFalse(pullRequest.state.buttons[1].answersOnATap, "request changes opens the diff")

        let authorize = try await store("perm-1", loaded: try Fixtures.authorizeCard())
        XCTAssertTrue(authorize.state.buttons.allSatisfy(\.answersOnATap))
    }
}
