import AlondraContract
import AlondraDesign
import AlondraFeatures
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
        var growsWithText = true
        @Namespace private var zoom

        var body: some View {
            ScreenFrame(height: height, growsWithText: growsWithText) { TriageView(store: store, zoom: zoom) }
        }
    }

    private func single(_ document: ViewDocument, edit: (inout TriageFeature.State) -> Void = { _ in }) throws
        -> StoreOf<TriageFeature> {
        let id = document.key.replacingOccurrences(of: "card:", with: "")
        return try ScreenStores.triage(queue: [id], cards: [document], edit: edit)
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
        assertVariants { Harness(store: ScreenStores.triage(state: state)) }
    }

    func test_empty() throws {
        let store = try ScreenStores.triage(queue: [])
        assertVariants { Harness(store: store) }
    }

    func test_empty_es() throws {
        let store = try ScreenStores.triage(queue: [])
        assertVariants(language: .spanish) { Harness(store: store) }
    }

    func test_offlineWithTheSavedQueue() throws {
        let store = try single(try Fixtures.card(Fixtures.questionID)) {
            $0.screen.phase = .failed("offline")
        }
        assertScreen(inWindow: true) { Harness(store: store) }
    }

    // MARK: One card of each kind

    func test_decide() throws {
        let store = try single(try Fixtures.card(Fixtures.questionID))
        assertScreen(inWindow: true) { Harness(store: store) }
    }

    func test_decide_es() throws {
        let store = try single(try Fixtures.card(Fixtures.questionID))
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
        let store = try single(try Fixtures.document("card-group-es"))
        assertScreen(language: .spanish, inWindow: true) { Harness(store: store) }
    }

    func test_decideWithAReplyBeingWritten() throws {
        let store = try single(try Fixtures.card(Fixtures.questionID)) {
            $0.cards[0].replyDraft = "Only if the title stays on one line"
        }
        assertScreen(inWindow: true) { Harness(store: store) }
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
        let store = try single(try Fixtures.authorizeCard())
        assertScreen(language: .spanish, inWindow: true) { Harness(store: store) }
    }

    func test_problem() throws {
        let store = try single(try Fixtures.problemCard())
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
        let store = try single(try Fixtures.planCard())
        assertScreen(language: .spanish, inWindow: true) { Harness(store: store) }
    }

    func test_reviewAPullRequestHeldBackByAnotherCard() throws {
        let store = try single(try Fixtures.card(Fixtures.pullRequestID))
        assertScreen(inWindow: true) { Harness(store: store) }
    }

    func test_reviewAPullRequestReadyToMerge() throws {
        let store = try single(try Fixtures.unblocked(try Fixtures.card(Fixtures.pullRequestID)))
        assertScreen(inWindow: true) { Harness(store: store) }
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

    func test_stackOfAShortAndALongCardShareOneHeight() throws {
        let queue = [Fixtures.informationID, Fixtures.questionID, Fixtures.deliveryID]
        let store = try ScreenStores.triage(queue: queue, cards: try queue.map(Fixtures.card))
        assertScreen(inWindow: true) { Harness(store: store) }
    }

    private func stackWithAFrontCard(
        in phase: ScreenFeature.State.Phase
    ) throws -> StoreOf<TriageFeature> {
        let queue = [Fixtures.questionID, "perm-1"]
        return try ScreenStores.triage(queue: queue, cards: [try Fixtures.authorizeCard()]) { state in
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
        let store = try stackWithAFrontCard(in: .loading)
        assertScreen(language: .spanish, inWindow: true) { Harness(store: store) }
    }

    func test_stackWhoseFrontCardFailedToLoad() throws {
        let store = try stackWithAFrontCard(in: .failed(TriageFeature.loadFailure))
        assertScreen(inWindow: true) { Harness(store: store) }
    }

    func test_stackWhoseFrontCardFailedToLoad_es() throws {
        let store = try stackWithAFrontCard(in: .failed(TriageFeature.loadFailure))
        assertScreen(language: .spanish, inWindow: true) { Harness(store: store) }
    }

    func test_stackOnASmallScreenAtTheLargestTextGivesEveryCardTheTallestHeight() throws {
        let queue = [Fixtures.questionID, Fixtures.informationID, Fixtures.deliveryID]
        let store = try ScreenStores.triage(queue: queue, cards: try queue.map(Fixtures.card))
        assertScreen([.light]) {
            Harness(store: store, height: 667).environment(\.dynamicTypeSize, .xxxLarge)
        }
    }

    func test_stackOnASmallScreenAtTheLargestAccessibilityTextScrollsWholeCards() throws {
        let queue = [Fixtures.questionID, Fixtures.informationID, Fixtures.deliveryID]
        let store = try ScreenStores.triage(queue: queue, cards: try queue.map(Fixtures.card))
        assertScreen([.light]) {
            Harness(store: store, height: 667, growsWithText: false).environment(\.dynamicTypeSize, .accessibility5)
        }
    }

    func test_stackScrolledToTheEndShowsTheLastButtonAboveTheBottomMargin() throws {
        let queue = [Fixtures.questionID, Fixtures.informationID, Fixtures.deliveryID]
        let store = try ScreenStores.triage(queue: queue, cards: try queue.map(Fixtures.card))
        assertScreen([.light]) {
            Harness(store: store, height: 667, growsWithText: false)
                .environment(\.dynamicTypeSize, .accessibility5)
                .defaultScrollAnchor(.bottom)
        }
    }

    func test_stackAfterTheLastCardWrapsToTheFirst() throws {
        let queue = [Fixtures.questionID, Fixtures.informationID, Fixtures.deliveryID, Fixtures.pullRequestID]
        let shown = [Fixtures.pullRequestID, Fixtures.questionID, Fixtures.informationID]
        let store = try ScreenStores.triage(queue: queue, cards: try shown.map(Fixtures.card)) { $0.index = 3 }
        assertScreen(inWindow: true) { Harness(store: store) }
    }

    // MARK: Claude usage has run out

    private func exhaustedHome(in seconds: TimeInterval, macsExhausted: Int = 1, macsTotal: Int = 1) throws -> ViewDocument {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        let until = formatter.string(from: Date(timeIntervalSince1970: 1_790_000_000).addingTimeInterval(seconds))
        return try Fixtures.document("home-ios") { object in
            var sections = try XCTUnwrap(object["sections"] as? [[String: Any]])
            sections[0]["card_ids"] = [Fixtures.questionID]
            object["sections"] = sections
            var status = try XCTUnwrap(object["status"] as? [String: Any])
            status["usage_exhausted"] = [
                "scope": macsTotal > 1 ? "all_macs" : "this_mac", "until": until,
                "macs_exhausted": macsExhausted, "macs_total": macsTotal,
            ] as [String: Any]
            object["status"] = status
        }
    }

    func test_usageExhausted() throws {
        let store = try ScreenStores.triage(
            queue: [Fixtures.questionID], cards: [try Fixtures.card(Fixtures.questionID)], home: try exhaustedHome(in: 2 * 3600)
        )
        assertScreen(inWindow: true) { Harness(store: store) }
    }

    func test_usageExhausted_es() throws {
        let store = try ScreenStores.triage(
            queue: [Fixtures.questionID], cards: [try Fixtures.card(Fixtures.questionID)], home: try exhaustedHome(in: 2 * 3600)
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
