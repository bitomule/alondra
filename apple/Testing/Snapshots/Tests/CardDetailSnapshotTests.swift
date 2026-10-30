import AlondraContract
import AlondraDesign
import AlondraFeatures
import AlondraiOSUI
import ComposableArchitecture
import SwiftUI
import XCTest

/// A card read in full: its content, context and history, with the card's own actions pinned at
/// the bottom; a document opened by itself; the sheet that comments on a block.
@MainActor
final class CardDetailSnapshotTests: XCTestCase {
    override func setUp() async throws { ScreenStores.pinTime() }

    /// Drawn in the key window, where the floating Liquid Glass bar and its scroll edge exist; drawn
    /// outside one, the bar's glass and its safe area render nothing.
    private func assertDetail(
        _ store: StoreOf<CardDetailFeature>,
        language: SnapshotLanguage = .english,
        testName: String = #function,
        line: UInt = #line
    ) {
        assertScreen(language: language, inWindow: true, testName: testName, line: line) {
            ScreenFrame(height: 852) { CardDetailView(store: store) }
        }
    }

    private let plan = """
    # Release plan

    The beta goes out on Friday once the three checks below pass.

    - Build the archive on the release machine
    - Upload to TestFlight and wait for processing
    - Send the notes to the internal group

    | Step | Owner |
    | --- | --- |
    | Archive | Alondra |
    | Notes | You |

    Roll back by pulling the build if crash reports exceed the threshold.
    """

    private let planSpanish = """
    # Plan de publicación

    La beta sale el viernes cuando pasen las tres comprobaciones de abajo.

    - Generar el archivo en la máquina de publicación
    - Subirlo a TestFlight y esperar a que se procese
    - Enviar las notas al grupo interno

    | Paso | Responsable |
    | --- | --- |
    | Archivo | Alondra |
    | Notas | Tú |
    """

    private func fetchedPlan(spanish: Bool = false, edit: (inout CardDetailFeature.State) -> Void = { _ in }) throws
        -> StoreOf<CardDetailFeature> {
        let card = spanish ? try Fixtures.spanish(try Fixtures.planCard()) : try Fixtures.planCard()
        return ScreenStores.cardDetail(card) { state in
            state.contentDigest = "plan-digest"
            state.content = spanish ? planSpanish : plan
            edit(&state)
        }
    }

    // MARK: Loading

    func test_loading() {
        assertDetail(Store(initialState: CardDetailFeature.State(key: .card("c-1"))) { CardDetailFeature() })
    }

    func test_noLongerAvailable() {
        var state = CardDetailFeature.State(key: .card("c-1"))
        state.screen.phase = .missing
        assertDetail(ScreenStores.cardDetail(state: state))
    }

    // MARK: Each kind of card in full

    func test_decide() throws {
        let store = ScreenStores.cardDetail(try Fixtures.card(Fixtures.questionID))
        assertDetail(store)
    }

    func test_decide_es() throws {
        let store = ScreenStores.cardDetail(try Fixtures.spanishCard(Fixtures.questionID))
        assertDetail(store, language: .spanish)
    }

    func test_aCardFromANewerDaemonAsksToUpdateForWhatItCannotUse() throws {
        assertDetail(ScreenStores.cardDetail(try Fixtures.newerDaemonCard()))
    }

    func test_aCardFromANewerDaemonAsksToUpdateForWhatItCannotUse_es() throws {
        assertDetail(ScreenStores.cardDetail(try Fixtures.newerDaemonCard(spanish: true)), language: .spanish)
    }

    func test_decideWithItsHistory() throws {
        let store = ScreenStores.cardDetail(try Fixtures.card(Fixtures.questionID).withHistory())
        assertDetail(store)
    }

    func test_authorize() throws {
        let store = ScreenStores.cardDetail(try Fixtures.authorizeCard())
        assertDetail(store)
    }

    func test_problem() throws {
        let store = ScreenStores.cardDetail(try Fixtures.problemCard())
        assertDetail(store)
    }

    func test_markdownInWhatTheCardNeedsAndWhy() throws {
        assertDetail(ScreenStores.cardDetail(try Fixtures.markdownCard()))
    }

    func test_know() throws {
        let store = ScreenStores.cardDetail(try Fixtures.card(Fixtures.informationID))
        assertDetail(store)
    }

    /// Send in the reply field of an orchestrator notice sends: no sheet with a second field opens.
    func test_orchestratorNoticeReplySent() async throws {
        let handoff = AsyncStream<Void>.makeStream()
        let store = ScreenStores.cardDetailWithHandoff(try Fixtures.orchestratorNotice(),
            onHandOff: { handoff.continuation.yield(()) })
        store.send(.replyChanged("Stop the Lumo node, it can wait"))
        let reply = try XCTUnwrap(store.subject?.replyAction)
        let send = store.send(.actionTapped(reply))
        await send.finish()
        handoff.continuation.finish()
        var events = handoff.stream.makeAsyncIterator()
        let handedOff: Void? = await events.next()
        XCTAssertNotNil(handedOff, "capture starts after the real sent reducer reaches its dismissal handoff")
        XCTAssertNil(store.composition)
        XCTAssertEqual(store.reply, "")
        XCTAssertEqual(store.actions.stamp, reply.stamp ?? String(localized: "Sent"))
        assertDetail(store)
    }

    func test_knowWithACommandInItsBody_es() throws {
        let store = ScreenStores.cardDetail(try Fixtures.document("card-know-command-es") { _ in })
        assertDetail(store, language: .spanish)
    }

    // MARK: How a request of the user ended, written by Alondra

    func test_outcomeAnswer() throws {
        let store = ScreenStores.cardDetail(try Fixtures.document("card-outcome-answer") { _ in })
        assertDetail(store)
    }

    func test_outcomeAnswer_es() throws {
        let store = ScreenStores.cardDetail(try Fixtures.document("card-outcome-answer-es") { _ in })
        assertDetail(store, language: .spanish)
    }

    func test_outcomeDone() throws {
        let store = ScreenStores.cardDetail(try Fixtures.document("card-outcome-done") { _ in })
        assertDetail(store)
    }

    func test_outcomeDone_es() throws {
        let store = ScreenStores.cardDetail(try Fixtures.document("card-outcome-done-es") { _ in })
        assertDetail(store, language: .spanish)
    }

    func test_pullRequestHeldBackByAnotherCard() throws {
        let store = ScreenStores.cardDetail(try Fixtures.card(Fixtures.pullRequestID))
        assertDetail(store)
    }

    func test_pullRequestReadyToMerge() throws {
        let store = ScreenStores.cardDetail(try Fixtures.unblocked(try Fixtures.card(Fixtures.pullRequestID)))
        assertDetail(store)
    }

    func test_pullRequestReadyToMerge_es() throws {
        let store = ScreenStores.cardDetail(try Fixtures.spanishCard(Fixtures.pullRequestID))
        assertDetail(store, language: .spanish)
    }

    // MARK: A final delivery held to its approved plan

    func test_deliveryWithItsApprovedPlan() throws {
        let store = ScreenStores.cardDetail(try Fixtures.unblocked(try Fixtures.document("card-delivery-checklist")))
        assertDetail(store)
    }

    func test_deliveryWithItsApprovedPlan_es() throws {
        let store = ScreenStores.cardDetail(try Fixtures.spanish(try Fixtures.unblocked(try Fixtures.document("card-delivery-checklist-es"))))
        assertDetail(store, language: .spanish)
    }

    // MARK: A research that ends in a plan card, or in a report that is only seen

    /// The screen reads the plan a card carries when it loads; the store starts already loaded.
    private func proposedPlan(_ document: ViewDocument) -> StoreOf<CardDetailFeature> {
        ScreenStores.cardDetail(document) { state in
            state.content = document.sections.lazy.flatMap(\.items).compactMap { item -> String? in
                if case .markdown(let markdown) = item, markdown.commentable { markdown.text } else { nil }
            }.first
        }
    }

    func test_deliveryThatProposesAPlan() throws {
        assertDetail(proposedPlan(try Fixtures.unblocked(try Fixtures.document("card-delivery-plan"))))
    }

    func test_deliveryThatProposesAPlan_es() throws {
        assertDetail(proposedPlan(try Fixtures.spanish(try Fixtures.unblocked(try Fixtures.document("card-delivery-plan-es")))), language: .spanish)
    }

    func test_reportAlone() throws {
        let store = ScreenStores.cardDetail(try Fixtures.unblocked(try Fixtures.document("card-delivery-report")))
        assertDetail(store)
    }

    func test_reportAlone_es() throws {
        let store = ScreenStores.cardDetail(try Fixtures.spanish(try Fixtures.unblocked(try Fixtures.document("card-delivery-report-es"))))
        assertDetail(store, language: .spanish)
    }

    func test_cardWithItsDocuments() throws {
        assertDetail(proposedPlan(try Fixtures.unblocked(try Fixtures.document("card-documents"))))
    }

    func test_cardWithItsDocuments_es() throws {
        assertDetail(proposedPlan(try Fixtures.spanish(try Fixtures.unblocked(try Fixtures.document("card-documents-es")))), language: .spanish)
    }

    // MARK: A plan to comment on

    func test_plan() throws {
        let store = try fetchedPlan()
        assertDetail(store)
    }

    func test_planWithComments() throws {
        let store = try fetchedPlan { state in
            for block in state.blocks.filter(\.isCommentable).prefix(2) {
                state.comments[block.anchor] = ActionComment(
                    anchor: block.anchor, text: "Say which simulator", quote: block.plainText
                )
            }
            state.reply = "Looks right overall."
        }
        assertDetail(store)
    }

    func test_planWithComments_es() throws {
        let store = try fetchedPlan(spanish: true) { state in
            if let block = state.blocks.first(where: \.isCommentable) {
                state.comments[block.anchor] = ActionComment(
                    anchor: block.anchor, text: "Indica el simulador", quote: block.plainText
                )
            }
        }
        assertDetail(store, language: .spanish)
    }

    func test_planFetching() throws {
        let store = ScreenStores.cardDetail(try Fixtures.planCard()) { $0.contentDigest = "plan-digest" }
        assertDetail(store)
    }

    func test_planThatCouldNotBeFetched() throws {
        let store = ScreenStores.cardDetail(try Fixtures.planCard()) { state in
            state.contentDigest = "plan-digest"
            state.contentFailed = true
        }
        assertDetail(store)
    }

    // MARK: A plan Claude Code holds, annotated

    private func heldPlan(annotating count: Int, spanish: Bool = false, edit: (inout CardDetailFeature.State) -> Void = { _ in }) throws
        -> StoreOf<CardDetailFeature> {
        let text = spanish ? planSpanish : plan
        let card = try Fixtures.heldPlanCard(text: text)
        return ScreenStores.cardDetail(spanish ? try Fixtures.spanish(card) : card) { state in
            state.content = text
            let notes = spanish ? ["Indica el simulador", "¿Quién aprueba las notas?"] : ["Say which simulator", "Who approves the notes?"]
            for (block, note) in zip(state.blocks.filter(\.isCommentable).dropFirst(), notes).prefix(count) {
                state.comments[block.anchor] = ActionComment(anchor: block.anchor, text: note, quote: block.plainText)
            }
            edit(&state)
        }
    }

    func test_heldPlan() throws {
        assertDetail(try heldPlan(annotating: 0))
    }

    func test_heldPlanWithTwoAnnotations() throws {
        assertDetail(try heldPlan(annotating: 2))
    }

    func test_heldPlanWithTwoAnnotations_es() throws {
        assertDetail(try heldPlan(annotating: 2, spanish: true), language: .spanish)
    }

    // MARK: A document opened by itself

    func test_documentFetching() throws {
        let store = ScreenStores.cardDetail(try Fixtures.documentRevision()) { state in
            state.contentDigest = "digest-1"
        }
        assertDetail(store)
    }

    func test_document() throws {
        let store = ScreenStores.cardDetail(try Fixtures.documentRevision()) { state in
            state.contentDigest = "digest-1"
            state.content = plan
        }
        assertDetail(store)
    }

    // MARK: Commenting

    func test_commentSheet() throws {
        let store = try fetchedPlan { state in
            let block = state.blocks.first(where: \.isCommentable)!
            state.composing = CardDetailFeature.Composing(
                anchor: block.anchor, quote: block.plainText, text: "Which simulator was it?"
            )
        }
        let composing = store.withState { $0.composing! }
        assertVariants { NavigationStack { CommentSheet(store: store, composing: composing) }.frame(height: 560) }
    }
}
