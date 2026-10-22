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
            $0.continuousClock = TestClock()
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

    @MainActor
    func testACardWhoseViewNeverArrivesBecomesAnErrorWithRetryInsteadOfSpinningForever() async throws {
        let clock = TestClock()
        let store = TestStore(initialState: TriageCardFeature.State(id: Fixtures.questionID)) { TriageCardFeature() } withDependencies: {
            $0.continuousClock = clock
            $0.viewsClient.observe = { _ in AsyncThrowingStream { _ in } }
        }
        store.exhaustivity = .off

        await store.send(.screen(.task))
        XCTAssertEqual(store.state.screen.phase, .loading)
        await clock.advance(by: TriageFeature.loadDeadline)
        await store.skipReceivedActions()

        XCTAssertEqual(store.state.screen.phase, .failed(TriageFeature.loadFailure))
        XCTAssertNil(store.state.card)

        await store.send(.screen(.retry))
        XCTAssertEqual(store.state.screen.phase, .loading)
        await clock.advance(by: TriageFeature.loadDeadline)
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.screen.phase, .failed(TriageFeature.loadFailure))
        await store.send(.screen(.stop))
    }

    @MainActor
    func testACardThatLoadsInTimeIsNeverFailedByTheDeadline() async throws {
        let clock = TestClock()
        let feed = FeedStub([try Fixtures.card(Fixtures.questionID)])
        let store = TestStore(initialState: TriageCardFeature.State(id: Fixtures.questionID)) { TriageCardFeature() } withDependencies: {
            $0.continuousClock = clock
            $0.viewsClient.observe = feed.observe
        }
        store.exhaustivity = .off

        await store.send(.screen(.task))
        await store.skipReceivedActions()
        await clock.advance(by: TriageFeature.loadDeadline * 2)

        XCTAssertEqual(store.state.screen.phase, .loaded)
        XCTAssertNotNil(store.state.card)
        await store.send(.screen(.stop))
    }

    // MARK: The buttons

    @MainActor
    func testTheButtonsAreTheDaemonsActionsInItsOrderAndMoreHoldsTheExtras() async throws {
        let store = try await store(Fixtures.questionID, loaded: try Fixtures.card(Fixtures.questionID))

        XCTAssertEqual(store.state.buttons.map(\.label), ["★ Ship the concise copy", "Keep the warmer copy"])
        XCTAssertEqual(store.state.buttons.map(\.role), [.advance, .neutral])
        XCTAssertEqual(store.state.menu.map(\.label), ["Request clarification"])
        XCTAssertEqual(store.state.replyAction?.id, "reply:\(Fixtures.questionID)@1")
    }

    @MainActor
    func testTheButtonsAndMoreAreTheDaemonsSplit() async throws {
        let store = try await store("decide-5", loaded: try Fixtures.crowdedDecision())

        XCTAssertEqual(store.state.buttons.count, 3)
        XCTAssertEqual(store.state.buttons.map(\.label), ["★ Stable", "Beta", "Nightly"])
        XCTAssertEqual(
            store.state.menu.map(\.label), ["Preview", "Internal", "Request clarification"]
        )
        XCTAssertEqual(store.state.allActions.count, 7, "three buttons, three in More, and the reply field")
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
        XCTAssertEqual(store.state.actions.error, "Your Mac didn’t confirm it. It may have arrived: try again and it won’t be done twice.")
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

    /// The haptic follows `sends`: it changes on the tap, before the intent is even prepared, and
    /// the Mac's answer does not change it again.
    @MainActor
    func testTheMergeIsFeltTheMomentItIsTappedAndNotWhenTheMacAnswers() async throws {
        let feed = FeedStub()
        let (preparing, prepared) = AsyncStream<Void>.makeStream()
        let (answering, answered) = AsyncStream<Void>.makeStream()
        var state = TriageCardFeature.State(id: Fixtures.pullRequestID)
        state.screen.document = try Fixtures.card(Fixtures.pullRequestID)
        let store = TestStore(initialState: state) { TriageCardFeature() } withDependencies: {
            $0.viewsClient.prepare = { request in
                for await _ in preparing { break }
                return request
            }
            $0.viewsClient.perform = { request in
                for await _ in answering { break }
                return try await feed.perform()(request)
            }
        }
        store.exhaustivity = .off
        let merge = try action(store, verb: "merge")

        await store.send(.menuOptionPicked(merge, optionID: "squash"))
        await store.receive(\.submit)
        await store.receive(\.actions.submit)
        XCTAssertEqual(store.state.actions.sends, 1, "the tap is felt at once")
        XCTAssertNil(store.state.stamp, "while the intent is prepared nothing has been answered yet")
        prepared.yield()
        await store.receive(\.actions.sent)
        answered.yield()
        await store.receive(\.actions.response)
        XCTAssertNotNil(store.state.stamp)
        XCTAssertEqual(store.state.actions.sends, 1, "the Mac's answer is not felt a second time")
    }

    @MainActor
    func testAMergeThatFailsTakesItsStampBackAndSaysWhyAfterTheTapWasFelt() async throws {
        var state = TriageCardFeature.State(id: Fixtures.pullRequestID)
        state.screen.document = try Fixtures.card(Fixtures.pullRequestID)
        let store = TestStore(initialState: state) { TriageCardFeature() } withDependencies: {
            $0.viewsClient.perform = { _ in throw URLError(.notConnectedToInternet) }
        }
        store.exhaustivity = .off
        let merge = try action(store, verb: "merge")

        await store.send(.menuOptionPicked(merge, optionID: "squash"))
        await store.receive(\.actions.submit)
        XCTAssertEqual(store.state.actions.sends, 1)
        await store.receive(\.actions.undelivered)
        await store.receive(\.actions.delegate.failed)
        XCTAssertNil(store.state.stamp, "the card shows again with its buttons")
        XCTAssertNotNil(store.state.actions.error)
        XCTAssertEqual(store.state.actions.sends, 1)
    }

    @MainActor
    func testAConfirmationIsFeltWhenConfirmedNotWhenAsked() async throws {
        let store = try await store(Fixtures.pullRequestID, loaded: try Fixtures.card(Fixtures.pullRequestID))
        let close = try XCTUnwrap(store.state.menu.first { $0.confirm != nil })

        await store.send(.actionTapped(close))
        await store.receive(\.actions.submit)
        XCTAssertNotNil(store.state.actions.confirmation)
        XCTAssertEqual(store.state.actions.sends, 0, "asking is not sending")
        await store.send(.actions(.confirm))
        XCTAssertEqual(store.state.actions.sends, 1)
    }

    @MainActor
    func testADisabledButtonSendsNothing() async throws {
        let feed = FeedStub()
        let store = try await store(Fixtures.pullRequestID, feed: feed, loaded: try Fixtures.card(Fixtures.pullRequestID))
        let merge = try ContractCodec.decode(AlondraContract.Action.self, from: Data(
            #"{"id":"merge:github.com/example/repo#12@1","label":"Merge","style":"primary","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":false,"disabled_reason":"Answer the question first"}"#.utf8))
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
    func testMergingSendsTheChosenMethodWithNoConfirmation() async throws {
        let feed = FeedStub()
        let pullRequest = try Fixtures.unblocked(try Fixtures.card(Fixtures.pullRequestID))
        let store = try await store(Fixtures.pullRequestID, feed: feed, loaded: pullRequest)
        let merge = try action(store, verb: "merge")
        XCTAssertEqual(merge.presentation, .menu)

        await store.send(.menuOptionPicked(merge, optionID: "squash"))
        await store.skipReceivedActions()
        XCTAssertNil(store.state.actions.confirmation)
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
    func testASentReplyCarriesTheFieldAndTheFieldIsClearedOnceApplied() async throws {
        let feed = FeedStub()
        let store = try await store(Fixtures.questionID, feed: feed, loaded: try Fixtures.card(Fixtures.questionID))
        let reply = try XCTUnwrap(store.state.replyAction)

        await store.send(.replyChanged("Ship the warmer one"))
        XCTAssertEqual(store.state.reply, "Ship the warmer one")
        await store.send(.replySent(reply))
        await store.skipReceivedActions()

        let request = try XCTUnwrap(feed.requests.first)
        XCTAssertEqual(request.actionId, reply.id)
        XCTAssertNil(request.optionId, "a reply answers no option")
        XCTAssertEqual(request.input.text, "Ship the warmer one")
        XCTAssertEqual(store.state.reply, "")
    }

    @MainActor
    func testEachReplySendsWhatTheFieldHoldsThenAndNothingWrittenBefore() async throws {
        let feed = FeedStub()
        let store = try await store(Fixtures.questionID, feed: feed, loaded: try Fixtures.card(Fixtures.questionID))
        let reply = try XCTUnwrap(store.state.replyAction)

        await store.send(.replyChanged("An older thought"))
        await store.send(.replyChanged("Ship the warmer one"))
        await store.send(.replySent(reply))
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.reply, "", "the field is empty after sending")

        await store.send(.replyChanged("And keep the title short"))
        await store.send(.replySent(reply))
        await store.skipReceivedActions()

        XCTAssertEqual(feed.requests.map(\.input.text), ["Ship the warmer one", "And keep the title short"])
        XCTAssertEqual(store.state.reply, "")
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
    func testAFailedReplyKeepsItsWordsAndReportsUpward() async throws {
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
        await store.receive(\.actions.undelivered)
        await store.receive(\.actions.delegate.failed)
        XCTAssertEqual(store.state.reply, "Ship the warmer one")
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
    func testTappingAnOptionAnswersAtOnceEvenWhenAnOlderDaemonMarkedItAsTakingANote() async throws {
        for input: [String: Any] in [["kind": "none"], ["kind": "text", "placeholder": "Add a note", "required": false, "multiline": true],
                                     ["kind": "text", "placeholder": "Add a note", "required": true, "multiline": true]] {
            let feed = FeedStub()
            let document = try Fixtures.card("card-question") { card in
                var actions = try XCTUnwrap(card["actions"] as? [[String: Any]])
                for index in actions.indices { actions[index]["input"] = input; actions[index]["attachments"] = input["kind"] as? String == "text" }
                card["actions"] = actions
            }
            let store = try await store(Fixtures.questionID, feed: feed, loaded: document)
            let done = try action(store, verb: "ans", label: "Keep the warmer copy")

            await store.send(.actionTapped(done))
            XCTAssertNil(store.state.composition, "\(input): an option never opens the sheet")
            await store.skipReceivedActions()

            let request = try XCTUnwrap(feed.requests.first, "\(input)")
            XCTAssertEqual(request.actionId, done.id)
            XCTAssertEqual(request.optionId, "warm")
            XCTAssertNil(request.input.text, "\(input)")
        }
    }

    @MainActor
    func testWhatIsWrittenInTheCardsFieldGoesWithTheOptionTapped() async throws {
        let feed = FeedStub()
        let store = try await store(Fixtures.questionID, feed: feed, loaded: try Fixtures.card(Fixtures.questionID))
        let warm = try action(store, verb: "ans", label: "Keep the warmer copy")

        await store.send(.replyChanged("Only for the onboarding screen"))
        await store.send(.actionTapped(warm))
        XCTAssertNil(store.state.composition)
        await store.skipReceivedActions()

        let request = try XCTUnwrap(feed.requests.first)
        XCTAssertEqual(request.actionId, warm.id)
        XCTAssertEqual(request.optionId, "warm")
        XCTAssertEqual(request.input.text, "Only for the onboarding screen")
        XCTAssertEqual(store.state.reply, "", "the words went with the answer")
    }

    @MainActor
    func testTheCardsFieldAloneAnswersInThePersonsOwnWords() async throws {
        let feed = FeedStub()
        let store = try await store(Fixtures.questionID, feed: feed, loaded: try Fixtures.card(Fixtures.questionID))
        let reply = try XCTUnwrap(store.state.replyAction)
        XCTAssertTrue(reply.id?.hasPrefix("reply:\(Fixtures.questionID)@") == true, "\(String(describing: reply.id))")
        XCTAssertFalse(store.state.allActions.contains { $0.label == "Write my own answer" }, "one field to write in, not two")

        await store.send(.replyChanged("Neither: keep the old copy"))
        await store.send(.replySent(reply))
        await store.skipReceivedActions()

        let request = try XCTUnwrap(feed.requests.first)
        XCTAssertEqual(request.actionId, reply.id)
        XCTAssertNil(request.optionId)
        XCTAssertEqual(request.input.text, "Neither: keep the old copy")
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
}
