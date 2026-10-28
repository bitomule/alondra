import AlondraContract
import AlondraDesign
import AlondraFeatures
import AlondraFeed
import AlondraTransport
import AlondraiOSUI
import ComposableArchitecture
import SwiftUI
import XCTest

/// Waiting for You in every state a person meets it in: waiting for the Mac, offline, empty, and
/// one card of each kind, with the buttons the daemon gives it.
@MainActor
final class TriageSnapshotTests: XCTestCase {
    override func setUp() async throws { ScreenStores.pinTime() }

    private struct Harness: View {
        let store: StoreOf<TriageFeature>
        var height: CGFloat = 852
        var tabBarHeight: CGFloat = 0
        var connection = ConnectionState.connected(.irohRelay)
        @Namespace private var zoom

        var body: some View {
            ScreenFrame(height: height, growsWithText: false) {
                TriageView(store: store, zoom: zoom)
                    .safeAreaInset(edge: .bottom, spacing: 0) {
                        if tabBarHeight > 0 { Color.red.frame(height: tabBarHeight) }
                    }
                    .connectionLostOverlay(connection, hasSavedData: store.screen.document != nil, retry: {},
                                           signIn: nil, settings: {}, pairAgain: nil, localNetworkSettings: nil)
                    .environment(\.connectionState, connection)
            }
        }
    }

    private func single(_ document: ViewDocument, spanish: Bool = false, edit: (inout TriageFeature.State) -> Void = { _ in }) throws
        -> StoreOf<TriageFeature> {
        let id = document.key.replacingOccurrences(of: "card:", with: "")
        return try ScreenStores.triage(queue: [id], cards: [document], home: try Fixtures.home(queue: [id], spanish: spanish), edit: edit)
    }

    // MARK: Before there is a queue

    func test_loading() {
        assertVariants { Harness(store: ScreenStores.triage(state: TriageFeature.State())) }
    }

    func test_loading_es() {
        assertVariants(language: .spanish) { Harness(store: ScreenStores.triage(state: TriageFeature.State())) }
    }

    func test_cannotReachTheMac() {
        var state = TriageFeature.State()
        state.screen.phase = .failed(TriageFeature.loadFailure)
        assertScreen([.light, .dark], inWindow: true) {
            Harness(store: ScreenStores.triage(state: state), connection: .unreachable(.macNotAnswering))
        }
    }

    func test_cannotReachTheMac_es() {
        var state = TriageFeature.State()
        state.screen.phase = .failed(TriageFeature.loadFailure)
        assertScreen([.light], language: .spanish, inWindow: true) {
            Harness(store: ScreenStores.triage(state: state), connection: .unreachable(.macNotAnswering))
        }
    }

    /// The Mac answers but this one view would not load: the screen's own failure, nothing about the connection.
    func test_theMacAnswersButThisViewWouldNotLoad() {
        var state = TriageFeature.State()
        state.screen.phase = .failed(TriageFeature.loadFailure)
        assertScreen([.light]) { Harness(store: ScreenStores.triage(state: state)) }
    }

    func test_empty() throws {
        let store = try ScreenStores.triage(queue: [])
        assertVariants { Harness(store: store) }
    }

    func test_empty_es() throws {
        let store = try ScreenStores.triage(queue: [], home: try Fixtures.home(queue: [], spanish: true))
        assertVariants(language: .spanish) { Harness(store: store) }
    }

    /// The daemon's own Spanish home, its front cards drawn from the cards it carries: nothing
    /// else was asked of the Mac.
    func test_theFrontCardsComeWithTheQueue_es() throws {
        let home = try Fixtures.document("home-ios-es")
        let queue = home.sections.first?.cardIds ?? []
        XCTAssertEqual(home.firstCards.map(\.key), queue.prefix(3).map { "card:\($0)" })
        let store = try ScreenStores.triage(queue: queue, cards: try home.firstCards.map(Fixtures.spanish), home: home)
        assertScreen([.light], language: .spanish, inWindow: true) { Harness(store: store) }
    }

    func test_offlineWithTheSavedQueue() throws {
        let store = try single(try Fixtures.card(Fixtures.questionID)) {
            $0.screen.phase = .failed("offline")
        }
        assertScreen(inWindow: true) { Harness(store: store, connection: .unreachable(.macNotAnswering)) }
    }

    // MARK: One connection state, every indicator

    /// 14 Oct, 08:58, iPhone on 5G: a reload of the queue had failed, but the Mac answered this
    /// iPhone 11 s before. The header and the dot say the same thing: the Mac is answering.
    func test_aFailedReloadWhileTheMacAnswersShowsTheLiveQueue() throws {
        let store = try single(try Fixtures.card(Fixtures.questionID)) { $0.screen.phase = .failed("offline") }
        assertScreen([.light, .dark], inWindow: true) { Harness(store: store, height: 320) }
    }

    func test_aFailedReloadWhileTheMacAnswersShowsTheLiveQueue_es() throws {
        let store = try single(try Fixtures.spanishCard(Fixtures.questionID), spanish: true) { $0.screen.phase = .failed("offline") }
        assertScreen([.light], language: .spanish, inWindow: true) { Harness(store: store, height: 320) }
    }

    func test_connectingWithTheSavedQueue() throws {
        let store = try single(try Fixtures.card(Fixtures.questionID))
        assertScreen([.light, .dark], inWindow: true) { Harness(store: store, height: 320, connection: .connecting) }
    }

    func test_connectingWithTheSavedQueue_es() throws {
        let store = try single(try Fixtures.spanishCard(Fixtures.questionID), spanish: true)
        assertScreen([.light], language: .spanish, inWindow: true) { Harness(store: store, height: 320, connection: .connecting) }
    }

    func test_theMacNotAnsweringWithTheSavedQueue() throws {
        let store = try single(try Fixtures.card(Fixtures.questionID))
        assertScreen([.light, .dark], inWindow: true) {
            Harness(store: store, height: 320, connection: .unreachable(.macNotAnswering))
        }
    }

    func test_theMacNotAnsweringWithTheSavedQueue_es() throws {
        let store = try single(try Fixtures.spanishCard(Fixtures.questionID), spanish: true)
        assertScreen([.light], language: .spanish, inWindow: true) {
            Harness(store: store, height: 320, connection: .unreachable(.macNotAnswering))
        }
    }

    func test_noInternetWithTheSavedQueue_es() throws {
        let store = try single(try Fixtures.spanishCard(Fixtures.questionID), spanish: true)
        assertScreen([.light], language: .spanish, inWindow: true) {
            Harness(store: store, height: 320, connection: .unreachable(.noInternet))
        }
    }

    func test_answersSetAsideNameTheCardWaitingAgain() throws {
        let store = try single(try Fixtures.card(Fixtures.questionID)) {
            $0.setAside = SetAsideAnswers(cards: [Fixtures.questionID], otherActions: 0, complete: true)
        }
        assertScreen([.light, .dark], inWindow: true) { Harness(store: store) }
    }

    func test_answersSetAsideNameTheCardWaitingAgain_es() throws {
        let store = try single(try Fixtures.spanishCard(Fixtures.questionID), spanish: true) {
            $0.setAside = SetAsideAnswers(cards: [Fixtures.questionID], otherActions: 0, complete: true)
        }
        assertScreen([.light], language: .spanish, inWindow: true) { Harness(store: store) }
    }

    func test_answersSetAsideFromAStoreThatShowedNothing() throws {
        let store = try single(try Fixtures.card(Fixtures.questionID)) {
            $0.setAside = SetAsideAnswers(cards: [], otherActions: 0, complete: false)
        }
        assertScreen([.light], inWindow: true) { Harness(store: store) }
    }

    func test_offlineWithTheSavedQueueSaysWhy() throws {
        let store = try single(try Fixtures.card(Fixtures.questionID)) { $0.screen.phase = .failed("offline") }
        assertScreen([.light, .dark], inWindow: true) {
            Harness(store: store, height: 320, connection: .unreachable(.localNetworkDenied))
                .environment(\.connectionRecovery, Self.recovery)
        }
    }

    func test_offlineWithTheSavedQueueSaysWhy_es() throws {
        let store = try single(try Fixtures.spanishCard(Fixtures.questionID), spanish: true) { $0.screen.phase = .failed("offline") }
        assertScreen([.light], language: .spanish, inWindow: true) {
            Harness(store: store, height: 320, connection: .unreachable(.localNetworkDenied))
                .environment(\.connectionRecovery, Self.recovery)
        }
    }

    private static let recovery = ConnectionRecovery(retry: {}, localNetworkSettings: {})

    // MARK: One card of each kind

    func test_decide() throws {
        let store = try single(try Fixtures.card(Fixtures.questionID))
        assertScreen(inWindow: true) { Harness(store: store) }
    }

    func test_decide_es() throws {
        let store = try single(try Fixtures.spanishCard(Fixtures.questionID), spanish: true)
        assertScreen(language: .spanish, inWindow: true) { Harness(store: store) }
    }

    func test_aCardFromANewerDaemonAsksToUpdateForWhatItCannotUse() throws {
        let store = try single(try Fixtures.newerDaemonCard())
        assertScreen(inWindow: true) { Harness(store: store) }
    }

    func test_aCardFromANewerDaemonAsksToUpdateForWhatItCannotUse_es() throws {
        let store = try single(try Fixtures.newerDaemonCard(spanish: true), spanish: true)
        assertScreen(language: .spanish, inWindow: true) { Harness(store: store) }
    }

    func test_decideWithMoreOptionsThanButtons() throws {
        let store = try single(try Fixtures.crowdedDecision())
        assertScreen(inWindow: true) { Harness(store: store) }
    }

    // MARK: One of several questions asked together

    func test_questionTwoOfThree() throws {
        let store = try single(try Fixtures.document("card-group"))
        assertScreen(inWindow: true) { Harness(store: store) }
    }

    func test_questionTwoOfThree_es() throws {
        let store = try single(try Fixtures.spanish(try Fixtures.document("card-group-es")), spanish: true)
        assertScreen(language: .spanish, inWindow: true) { Harness(store: store) }
    }

    func test_decideWithAReplyBeingWritten() throws {
        let store = try single(try Fixtures.card(Fixtures.questionID)) {
            $0.cards[0].reply = "Only if the title stays on one line"
        }
        assertScreen(inWindow: true) { Harness(store: store) }
    }

    func test_preparingActionKeepsItsCardAndWrittenReply() throws {
        let document = try Fixtures.card(Fixtures.questionID)
        let reply = "Only if the title stays on one line"
        let store = try single(document) { state in
            guard let action = state.cards[0].replyAction, let actionID = action.id else {
                XCTFail("The preparing fixture needs the card’s real reply action")
                return
            }
            state.cards[0].reply = reply
            state.cards[0].actions.action = action
            state.cards[0].actions.requiresAttachments = action.attachments
            state.cards[0].actions.text = reply
            state.cards[0].actions.pending = ActionRequest(
                commandId: "00000000-0000-4000-8000-000000000001", dataEpoch: document.dataEpoch,
                client: .ios, lang: "en", actionId: actionID, input: ActionInput(text: reply),
                resolvesCard: action.resolvesCard)
            state.cards[0].actions.inFlight = true
            state.cards[0].actions.stamp = nil
        }
        XCTAssertEqual(store.state.orderedIDs, [Fixtures.questionID])
        XCTAssertEqual(store.state.cards[0].reply, reply)
        XCTAssertEqual(store.state.cards[0].actions.text, reply)
        XCTAssertEqual(store.state.cards[0].actions.pending?.dataEpoch, document.dataEpoch)
        XCTAssertEqual(store.state.cards[0].actions.pending?.commandId, "00000000-0000-4000-8000-000000000001")
        XCTAssertTrue(store.state.cards[0].actions.inFlight)
        XCTAssertNil(store.state.cards[0].actions.stamp, "the written reply remains visible while its intent is being prepared")
        XCTAssertFalse(store.state.cards[0].actions.canSubmit)
        XCTAssertNil(store.state.cards[0].actions.unsure)
        assertScreen(inWindow: true) { Harness(store: store) }
    }

    func test_unconfirmedActionKeepsItsCardAndWrittenReply() throws {
        let document = try Fixtures.card(Fixtures.questionID)
        let reply = "Only if the title stays on one line"
        let store = try single(document) { state in
            state.cards[0].reply = reply
            guard let action = state.cards[0].replyAction ?? state.cards[0].buttons.first,
                  let actionID = action.id else { XCTFail("The fixture needs a real card action"); return }
            state.cards[0].actions.unsure = ActionRequest(
                commandId: "00000000-0000-4000-8000-000000000001", dataEpoch: document.dataEpoch,
                client: .ios, lang: "en", actionId: actionID, input: ActionInput(text: reply))
            state.cards[0].actions.error = OperationError.outcomeUnknown.localizedDescription
        }
        XCTAssertEqual(store.state.orderedIDs, [Fixtures.questionID])
        XCTAssertEqual(store.state.cards[0].reply, reply)
        XCTAssertEqual(store.state.cards[0].actions.unsure?.dataEpoch, document.dataEpoch)
        XCTAssertFalse(store.state.cards[0].actions.inFlight)
        assertScreen(inWindow: true, pixelPrecision: 0.9999) { Harness(store: store) }
    }

    func test_decideAnswered() throws {
        let store = try single(try Fixtures.card(Fixtures.questionID)) { state in
            state.cards[0].actions.action = state.cards[0].buttons.first
            state.cards[0].actions.stamp = "Answered"
        }
        assertScreen(inWindow: true) { Harness(store: store) }
    }

    func test_authorize() throws {
        let store = try single(try Fixtures.authorizeCard())
        assertScreen(inWindow: true) { Harness(store: store) }
    }

    func test_authorize_es() throws {
        let store = try single(try Fixtures.spanish(try Fixtures.authorizeCard()), spanish: true)
        assertScreen(language: .spanish, inWindow: true) { Harness(store: store) }
    }

    func test_problem() throws {
        let store = try single(try Fixtures.problemCard())
        assertScreen(inWindow: true) { Harness(store: store) }
    }

    func test_markdownInWhatTheCardNeedsAndWhy() throws {
        let store = try single(try Fixtures.markdownCard())
        assertScreen(inWindow: true) { Harness(store: store) }
    }

    func test_know() throws {
        let store = try single(try Fixtures.card(Fixtures.informationID))
        assertScreen(inWindow: true) { Harness(store: store) }
    }

    func test_reviewAPlan() throws {
        let store = try single(try Fixtures.planCard())
        assertScreen(inWindow: true) { Harness(store: store) }
    }

    func test_reviewAPlan_es() throws {
        let store = try single(try Fixtures.spanish(try Fixtures.planCard()), spanish: true)
        assertScreen(language: .spanish, inWindow: true) { Harness(store: store) }
    }

    func test_reviewAPullRequestReadyToMerge() throws {
        let store = try single(try Fixtures.unblocked(try Fixtures.card(Fixtures.pullRequestID)))
        assertScreen(inWindow: true) { Harness(store: store) }
    }

    func test_reviewAPullRequestReadyToMerge_es() throws {
        let store = try single(try Fixtures.spanishCard(Fixtures.pullRequestID), spanish: true)
        assertScreen(language: .spanish, inWindow: true) { Harness(store: store) }
    }

    /// Once the merge is sent the stamp covers the whole card: no button shows through it.
    func test_aMergeSentHidesTheCardButtonsUnderTheStamp() throws {
        for spanish in [false, true] {
            let store = try pullRequest(spanish: spanish) { actions, card in
                actions.action = try XCTUnwrap(card.actions.first { $0.id?.hasPrefix("merge:") == true })
                actions.stamp = spanish ? "Haciendo merge…" : "Merging…"
            }
            assertScreen([.light, .dark], language: language(spanish), inWindow: true) { Harness(store: store) }
        }
    }

    // MARK: What an action asks first, and what it says after

    private static let changedReply = [
        false: #"{"protocol":1,"outcome":"changed","result":{"kind":"changed","title":"PR #12 has new commits","body":"You reviewed an older version. Send anyway, or open the PR to see what changed.","next":{"id":"changes:github.com/example/repo#12@7~fedcba98","label":"Send anyway","style":"primary","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":true}}}"#,
        true: #"{"protocol":1,"outcome":"changed","result":{"kind":"changed","title":"La PR #12 tiene commits nuevos","body":"Revisaste una versión anterior. Envíalo igualmente o abre la PR para ver qué cambió.","next":{"id":"changes:github.com/example/repo#12@7~fedcba98","label":"Enviar igualmente","style":"primary","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":true}}}"#,
    ]

    private static let mergingReply = [
        false: #"{"protocol":1,"outcome":"applied","stamp":"Merging…","result":{"kind":"started","title":"The PR is merging","body":"You'll see it finished in its task when GitHub confirms."}}"#,
        true: #"{"protocol":1,"outcome":"applied","stamp":"Haciendo merge…","result":{"kind":"started","title":"Se está haciendo merge de la PR","body":"La verás terminada en su tarea cuando GitHub lo confirme."}}"#,
    ]

    /// The pull request card with its action state edited, in either language.
    private func pullRequest(spanish: Bool, _ edit: (inout ActionFeature.State, Card) throws -> Void) throws -> StoreOf<TriageFeature> {
        let document = spanish ? try Fixtures.spanishCard(Fixtures.pullRequestID) : try Fixtures.card(Fixtures.pullRequestID)
        let card = try XCTUnwrap(document.sections.flatMap(\.items).compactMap { item -> Card? in
            if case .card(let card) = item { card } else { nil }
        }.first)
        var actions = ActionFeature.State.device(dataEpoch: document.dataEpoch)
        try edit(&actions, card)
        return try single(document, spanish: spanish) { $0.cards[id: Fixtures.pullRequestID]?.actions = actions }
    }

    private func language(_ spanish: Bool) -> SnapshotLanguage { spanish ? .spanish : .english }

    func test_closingWithoutMergingAsksFirstInRed() async throws {
        for spanish in [false, true] {
            let store = try pullRequest(spanish: spanish) { actions, card in
                let close = try XCTUnwrap(card.more.first { $0.id?.hasPrefix("close_unmerged:") == true })
                actions.action = close
                actions.confirmation = close.confirm
            }
            await assertPresented(language: language(spanish), named: spanish ? "light_es" : "light") { Harness(store: store) }
        }
    }

    func test_aPullRequestThatChangedAsksBeforeSendingAnyway() async throws {
        for spanish in [false, true] {
            let reply = try ContractCodec.decode(ActionResponse.self, from: Data(try XCTUnwrap(Self.changedReply[spanish]).utf8))
            guard case .ask(let changed) = OutcomeStyle(reply) else { return XCTFail("a changed answer asks") }
            let store = try pullRequest(spanish: spanish) { actions, card in
                actions.action = card.actions.first { $0.id?.hasPrefix("changes:") == true }
                actions.changed = changed
            }
            await assertPresented(language: language(spanish), named: spanish ? "light_es" : "light") { Harness(store: store) }
        }
    }

    func test_theNoticeSaysWhatTheServerSaid() throws {
        for spanish in [false, true] {
            let reply = try ContractCodec.decode(ActionResponse.self, from: Data(try XCTUnwrap(Self.mergingReply[spanish]).utf8))
            guard case .success(let title) = OutcomeStyle(reply) else { return XCTFail("a started merge is a notice") }
            var app = AppFeature.State()
            app.notice = WorkspaceNotice(event: .actionCompleted(title))
            let notices = Store(initialState: app) { EmptyReducer<AppFeature.State, AppFeature.Action>() }
            let store = try single(spanish ? try Fixtures.spanishCard(Fixtures.questionID) : try Fixtures.card(Fixtures.questionID), spanish: spanish)
            assertScreen([.light], language: language(spanish), inWindow: true) {
                ZStack(alignment: .bottom) {
                    Harness(store: store)
                    NoticeToast(store: notices)
                }
            }
        }
    }

    /// A notification for a card answered elsewhere: the card is never drawn, the queue shows what
    /// still waits and the toast says what happened, in the words the daemon sent.
    func test_aSettledCardIsNotShownAndTheToastSaysWhy() throws {
        for spanish in [false, true] {
            let settled = try Fixtures.document(spanish ? "card-settled-ios-es" : "card-settled-ios")
            let notices = Store(initialState: AppFeature.State()) { AppFeature() } withDependencies: {
                $0.continuousClock = TestClock()
            }
            notices.send(.triage(.delegate(.noLongerPending(settled.settledNotice))))
            XCTAssertEqual(notices.notice?.event, .actionCompleted(spanish ? "Ya respondida" : "Already answered"))
            let waiting = spanish ? try Fixtures.spanishCard(Fixtures.informationID) : try Fixtures.card(Fixtures.informationID)
            let store = try single(waiting, spanish: spanish)
            assertScreen([.light], language: language(spanish), inWindow: true) {
                ZStack(alignment: .bottom) {
                    Harness(store: store)
                    NoticeToast(store: notices)
                }
            }
        }
    }

    // MARK: An answer that did not go through

    /// An answer the Mac never confirmed: its card waits again, plain, with nothing about the
    /// retry inside it, and the toast says which answer did not arrive.
    func test_anUnconfirmedAnswerBringsItsCardBackAndTheToastSaysWhich() throws {
        for spanish in [false, true] {
            let document = spanish ? try Fixtures.spanishCard(Fixtures.questionID) : try Fixtures.card(Fixtures.questionID)
            let store = try single(document, spanish: spanish) { state in
                let request = ActionRequest(commandId: "00000000-0000-4000-8000-000000000001", dataEpoch: document.dataEpoch,
                                            client: .ios, lang: spanish ? "es" : "en", actionId: "answer:\(Fixtures.questionID)@1", resolvesCard: true)
                state.optimistic.begin(Fixtures.questionID, request)
                state.optimistic.expire(Fixtures.questionID)
            }
            XCTAssertEqual(store.state.orderedIDs, [Fixtures.questionID])
            let card = try XCTUnwrap(store.state.currentCard)
            let title = card.need ?? card.title
            let message = spanish ? "Tu respuesta a «\(title)» no ha llegado a tu Mac."
                : Optimistic.notConfirmed(title)
            let notices = Store(initialState: AppFeature.State()) { AppFeature() } withDependencies: {
                $0.continuousClock = TestClock()
            }
            notices.send(.triage(.delegate(.answerNotConfirmed(message))))
            XCTAssertEqual(notices.notice?.event, .failure(message))
            assertScreen([.light], language: language(spanish), inWindow: true) {
                ZStack(alignment: .bottom) {
                    Harness(store: store)
                    NoticeToast(store: notices)
                }
            }
        }
    }

    func test_aPullRequestBackSaysWhy() throws {
        let document = try Fixtures.card(Fixtures.pullRequestID)
        let store = try single(document) {
            $0.cards[id: Fixtures.pullRequestID]?.actions.error =
                "Your Mac didn’t confirm it. It may have arrived: try again and it won’t be done twice."
        }
        assertScreen(inWindow: true) { Harness(store: store) }
    }

    func test_aPullRequestBackSaysWhy_es() throws {
        let store = try single(try Fixtures.spanishCard(Fixtures.pullRequestID), spanish: true) {
            $0.cards[id: Fixtures.pullRequestID]?.actions.error =
                "Tu Mac no lo confirmó. Puede que llegara: vuelve a intentarlo y no se hará dos veces."
        }
        assertScreen(language: .spanish, inWindow: true) { Harness(store: store) }
    }

    // MARK: Asking the node to clarify

    private func sheet(_ composition: TriageCardFeature.Composition) -> some View {
        CompositionSheet(
            composition: composition, tint: Palette.dawn,
            reasonChanged: { _ in }, noteChanged: { _ in }, cancel: {}, send: {}
        )
        .background(Palette.night)
    }

    func test_clarifySheet() throws {
        var composition = TriageCardFeature.Composition(action: try XCTUnwrap(
            try Fixtures.card(Fixtures.questionID).firstCard?.more.first { $0.id?.hasPrefix("clarify:") == true }
        ))
        composition.reason = "contradicts"
        composition.note = "The two options say opposite things"
        assertScreenVariants(inNavigationStack: false) { sheet(composition) }
    }

    // MARK: A queue

    func test_stackOfFiveWithThePageDots() throws {
        let queue = [Fixtures.questionID, Fixtures.informationID, Fixtures.deliveryID, Fixtures.pullRequestID]
        let store = try ScreenStores.triage(queue: queue, cards: try queue.prefix(3).map(Fixtures.card))
        assertScreen(inWindow: true) { Harness(store: store) }
    }

    /// A question arrived while the report was in front: the report stays, the question waits right
    /// behind it, and the header says there is one new card.
    func test_aCardThatArrivedWhileReadingIsAnnounced() throws {
        let queue = [Fixtures.deliveryID, Fixtures.questionID, Fixtures.informationID]
        let store = try ScreenStores.triage(queue: queue, cards: try queue.map(Fixtures.card)) { $0.arrivals = [Fixtures.questionID] }
        assertScreen(inWindow: true) { Harness(store: store) }
    }

    func test_aCardThatArrivedWhileReadingIsAnnounced_es() throws {
        let queue = [Fixtures.deliveryID, Fixtures.questionID, Fixtures.informationID]
        let store = try ScreenStores.triage(queue: queue, cards: try queue.map(Fixtures.spanishCard),
                                            home: try Fixtures.home(queue: queue, spanish: true)) { $0.arrivals = [Fixtures.questionID] }
        assertScreen([.light, .dark], language: .spanish, inWindow: true) { Harness(store: store) }
    }

    func test_stackOfAShortAndALongCardShareOneHeight() throws {
        let queue = [Fixtures.informationID, Fixtures.questionID, Fixtures.deliveryID]
        let store = try ScreenStores.triage(queue: queue, cards: try queue.map(Fixtures.card))
        assertScreen(inWindow: true) { Harness(store: store) }
    }

    private func stackWithAFrontCard(
        in phase: ScreenFeature.State.Phase, spanish: Bool = false
    ) throws -> StoreOf<TriageFeature> {
        let queue = [Fixtures.questionID, "perm-1"]
        let behind = spanish ? try Fixtures.spanish(try Fixtures.authorizeCard()) : try Fixtures.authorizeCard()
        return try ScreenStores.triage(queue: queue, cards: [behind], home: try Fixtures.home(queue: queue, spanish: spanish)) { state in
            var front = TriageCardFeature.State(id: Fixtures.questionID)
            front.screen.phase = phase
            state.cards.append(front)
        }
    }

    func test_stackWhoseFrontCardIsStillLoading() throws {
        let store = try stackWithAFrontCard(in: .loading)
        assertScreen(inWindow: true) { Harness(store: store) }
    }

    func test_stackWhoseFrontCardIsStillLoading_es() throws {
        let store = try stackWithAFrontCard(in: .loading, spanish: true)
        assertScreen(language: .spanish, inWindow: true) { Harness(store: store) }
    }

    func test_stackWhoseFrontCardFailedToLoad() throws {
        let store = try stackWithAFrontCard(in: .failed(TriageFeature.loadFailure))
        assertScreen(inWindow: true) { Harness(store: store) }
    }

    func test_stackWhoseFrontCardFailedToLoad_es() throws {
        let store = try stackWithAFrontCard(in: .failed(TriageFeature.loadFailure), spanish: true)
        assertScreen(language: .spanish, inWindow: true) { Harness(store: store) }
    }

    private static let longNeed = "Decide whether the release goes out on the beta channel this evening or waits for the "
        + "full regression pass that the QA team asked for last week, because both cannot happen together"
    private static let longWhy = "The regression pass touches the same migration the beta ships, so shipping first means "
        + "every tester runs the old schema while the new one is being verified, and rolling back afterwards "
        + "would need a manual export for each of them, which nobody has time for before the weekend."

    private static let longNeedSpanish = "Decide si la publicación sale hoy por la tarde en el canal beta o espera a "
        + "la pasada de regresión completa que pidió el equipo de QA la semana pasada, porque no pueden ir juntas"
    private static let longWhySpanish = "La pasada de regresión toca la misma migración que lleva la beta: si sale antes, "
        + "cada tester usa el esquema viejo mientras se verifica el nuevo, y volver atrás después exigiría una "
        + "exportación manual para cada uno, y nadie tiene tiempo antes del fin de semana."

    private func stackWithALongCard(spanish: Bool = false) throws -> StoreOf<TriageFeature> {
        let long = try Fixtures.card(spanish ? "card-question-es" : "card-question") { item in
            item["need"] = spanish ? Self.longNeedSpanish : Self.longNeed
            item["title"] = spanish ? Self.longNeedSpanish : Self.longNeed
            item["why"] = spanish ? Self.longWhySpanish : Self.longWhy
        }
        let queue = [Fixtures.questionID, Fixtures.informationID, Fixtures.deliveryID]
        let cards = spanish
            ? [try Fixtures.spanish(long), try Fixtures.spanishCard(Fixtures.informationID), try Fixtures.spanishCard(Fixtures.deliveryID)]
            : [long, try Fixtures.card(Fixtures.informationID), try Fixtures.card(Fixtures.deliveryID)]
        return try ScreenStores.triage(queue: queue, cards: cards, home: try Fixtures.home(queue: queue, spanish: spanish))
    }

    func test_stackWithAVeryLongCardKeepsItsHeightAndItsButtons() throws {
        let store = try stackWithALongCard()
        assertScreen([.light, .ax3], inWindow: true) { Harness(store: store) }
    }

    func test_stackWithAVeryLongCardKeepsItsHeightAndItsButtons_es() throws {
        let store = try stackWithALongCard(spanish: true)
        assertScreen([.light, .ax3], language: .spanish, inWindow: true) { Harness(store: store) }
    }

    func test_stackOnASmallScreenAtTheLargestTextStaysOneHeightWithItsButtons() throws {
        let store = try stackWithALongCard()
        assertScreen([.light], inWindow: true) {
            Harness(store: store, height: 667).environment(\.dynamicTypeSize, .xxxLarge)
        }
    }

    func test_stackOnASmallScreenAtTheLargestAccessibilityTextDoesNotScroll() throws {
        let store = try stackWithALongCard()
        assertScreen([.light], inWindow: true) {
            Harness(store: store, height: 667).environment(\.dynamicTypeSize, .accessibility5)
        }
    }

    func test_stackLeavesTheBottomMarginAboveTheTabBar() throws {
        let store = try stackWithALongCard()
        assertScreen([.light], inWindow: true) {
            Harness(store: store, height: 667, tabBarHeight: 83)
        }
    }

    func test_aCardAloneIsAsTallAsTheFrontOfAStack() throws {
        let alone = try single(try Fixtures.card(Fixtures.questionID))
        let queue = [Fixtures.questionID, Fixtures.informationID, Fixtures.deliveryID]
        let stacked = try ScreenStores.triage(queue: queue, cards: try queue.map(Fixtures.card))
        assertScreen([.light], inWindow: true) {
            VStack(spacing: 0) {
                Harness(store: alone, tabBarHeight: 83)
                Harness(store: stacked, tabBarHeight: 83)
            }
        }
    }

    func test_stackAfterTheLastCardWrapsToTheFirst() throws {
        let queue = [Fixtures.questionID, Fixtures.informationID, Fixtures.deliveryID, Fixtures.pullRequestID]
        let shown = [Fixtures.pullRequestID, Fixtures.questionID, Fixtures.informationID]
        let store = try ScreenStores.triage(queue: queue, cards: try shown.map(Fixtures.card)) { $0.index = 3 }
        assertScreen(inWindow: true) { Harness(store: store) }
    }

    // MARK: Claude usage has run out

    private func exhaustedHome(in seconds: TimeInterval, macsExhausted: Int = 1, macsTotal: Int = 1, pausedByUser: Bool = false,
                               spanish: Bool = false) throws -> ViewDocument {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        let until = formatter.string(from: Date(timeIntervalSince1970: 1_790_000_000).addingTimeInterval(seconds))
        return try Fixtures.document(spanish ? "home-ios-es" : "home-ios") { object in
            var sections = try XCTUnwrap(object["sections"] as? [[String: Any]])
            sections[0]["card_ids"] = [Fixtures.questionID]
            object["sections"] = sections
            var status = try XCTUnwrap(object["status"] as? [String: Any])
            status["usage_exhausted"] = [
                "scope": macsTotal > 1 ? "all_macs" : "this_mac", "until": until,
                "macs_exhausted": macsExhausted, "macs_total": macsTotal,
                "paused_by_user": pausedByUser,
            ] as [String: Any]
            object["status"] = status
        }
    }

    private func sessionWaitHome(spanish: Bool = false) throws -> ViewDocument {
        try Fixtures.document(spanish ? "home-ios-es" : "home-ios") { object in
            var sections = try XCTUnwrap(object["sections"] as? [[String: Any]])
            sections[0]["card_ids"] = [Fixtures.questionID]
            object["sections"] = sections
            var status = try XCTUnwrap(object["status"] as? [String: Any])
            status["session_wait"] = [
                "text": spanish
                    ? "Esperando una sesión de Claude · inicia sesión en una cuenta en Cuentas de Claude"
                    : "Waiting for a Claude session · sign in to an account in Claude accounts",
                "tone": "warning",
            ] as [String: Any]
            object["status"] = status
        }
    }

    func test_sessionWait() throws {
        let store = try ScreenStores.triage(
            queue: [Fixtures.questionID], cards: [try Fixtures.card(Fixtures.questionID)], home: try sessionWaitHome()
        )
        assertScreen(inWindow: true) { Harness(store: store) }
    }

    func test_sessionWait_es() throws {
        let store = try ScreenStores.triage(
            queue: [Fixtures.questionID], cards: [try Fixtures.spanishCard(Fixtures.questionID)], home: try sessionWaitHome(spanish: true)
        )
        assertScreen(language: .spanish, inWindow: true) { Harness(store: store) }
    }

    func test_usageExhausted() throws {
        let store = try ScreenStores.triage(
            queue: [Fixtures.questionID], cards: [try Fixtures.card(Fixtures.questionID)], home: try exhaustedHome(in: 2 * 3600)
        )
        assertScreen(inWindow: true) { Harness(store: store) }
    }

    func test_usageExhausted_es() throws {
        let store = try ScreenStores.triage(
            queue: [Fixtures.questionID], cards: [try Fixtures.spanishCard(Fixtures.questionID)], home: try exhaustedHome(in: 2 * 3600, spanish: true)
        )
        assertScreen(language: .spanish, inWindow: true) { Harness(store: store) }
    }

    func test_usageExhausted_pausedByUser() throws {
        let store = try ScreenStores.triage(
            queue: [Fixtures.questionID], cards: [try Fixtures.card(Fixtures.questionID)],
            home: try exhaustedHome(in: 5 * 3600, pausedByUser: true)
        )
        assertScreen(inWindow: true) { Harness(store: store) }
    }

    func test_usageExhausted_pausedByUser_es() throws {
        let store = try ScreenStores.triage(
            queue: [Fixtures.questionID], cards: [try Fixtures.spanishCard(Fixtures.questionID)],
            home: try exhaustedHome(in: 5 * 3600, pausedByUser: true, spanish: true)
        )
        assertScreen(language: .spanish, inWindow: true) { Harness(store: store) }
    }

    func test_usageExhausted_someMacsWithMinutesLeft() throws {
        let store = try ScreenStores.triage(
            queue: [Fixtures.questionID], cards: [try Fixtures.card(Fixtures.questionID)],
            home: try exhaustedHome(in: 25 * 60, macsExhausted: 2, macsTotal: 3)
        )
        assertScreen(inWindow: true) { Harness(store: store) }
    }
}

extension ViewDocument {
    fileprivate var firstCard: Card? {
        sections.flatMap(\.items).compactMap { item -> Card? in
            if case .card(let card) = item { card } else { nil }
        }.first
    }
}
