import AlondraContract
import AlondraFeatures
import AlondraiOSUI
import ComposableArchitecture
import SwiftUI
import XCTest

/// Waiting for You in every state a person meets it in: waiting for the Mac, offline, empty, and
/// one card of each kind.
@MainActor
final class TriageSnapshotTests: XCTestCase {
    override func setUp() async throws { ScreenStores.pinTime() }

    private struct Harness: View {
        let store: StoreOf<TriageFeature>
        @Namespace private var zoom

        var body: some View {
            NavigationStack { TriageView(store: store, zoom: zoom) }
                .frame(height: 852)
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
        state.screen.phase = .failed("Couldn’t load this from your Mac. Try again.")
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
        assertVariants { Harness(store: store) }
    }

    // MARK: One card of each kind

    func test_question() throws {
        let store = try single(try Fixtures.card(Fixtures.questionID))
        assertVariants { Harness(store: store) }
    }

    func test_question_es() throws {
        let store = try single(try Fixtures.card(Fixtures.questionID))
        assertVariants(language: .spanish) { Harness(store: store) }
    }

    func test_questionWithAPickedOptionAndAReply() throws {
        let store = try single(try Fixtures.card(Fixtures.questionID)) { state in
            let answer = state.cards[0].replyAction
            state.cards[0].actions.action = answer
            state.cards[0].actions.selectedOptionID = answer?.options.first?.id
            state.cards[0].replyDraft = "Only if the title stays on one line"
        }
        assertVariants { Harness(store: store) }
    }

    func test_questionAnswered() throws {
        let store = try single(try Fixtures.card(Fixtures.questionID)) { state in
            state.cards[0].actions.action = state.cards[0].replyAction
            state.cards[0].actions.stamp = "Answered"
        }
        assertVariants { Harness(store: store) }
    }

    func test_report() throws {
        let store = try single(try Fixtures.card(Fixtures.informationID))
        assertVariants { Harness(store: store) }
    }

    func test_delivery() throws {
        let store = try single(try Fixtures.liveDelivery())
        assertVariants { Harness(store: store) }
    }

    func test_delivery_es() throws {
        let store = try single(try Fixtures.liveDelivery())
        assertVariants(language: .spanish) { Harness(store: store) }
    }

    func test_pullRequest() throws {
        let store = try single(try Fixtures.card(Fixtures.pullRequestID))
        assertVariants { Harness(store: store) }
    }

    func test_permissionPrompt() throws {
        let store = try single(try Fixtures.permissionCard())
        assertVariants { Harness(store: store) }
    }

    func test_permissionPromptDenying() throws {
        let store = try single(try Fixtures.permissionCard()) { state in
            let answer = state.cards[0].replyAction
            state.cards[0].actions.action = answer
            state.cards[0].actions.selectedOptionID = "deny"
        }
        assertVariants { Harness(store: store) }
    }

    func test_plan() throws {
        let store = try single(try Fixtures.planCard())
        assertVariants { Harness(store: store) }
    }

    // MARK: A queue

    func test_stackOfFourWithThePageDots() throws {
        let queue = [Fixtures.questionID, Fixtures.informationID, Fixtures.deliveryID, Fixtures.pullRequestID]
        let store = try ScreenStores.triage(queue: queue, cards: try queue.prefix(3).map(Fixtures.card))
        assertVariants { Harness(store: store) }
    }

    func test_stackAfterTheLastCardWrapsToTheFirst() throws {
        let queue = [Fixtures.questionID, Fixtures.informationID, Fixtures.deliveryID, Fixtures.pullRequestID]
        let shown = [Fixtures.pullRequestID, Fixtures.questionID, Fixtures.informationID]
        let store = try ScreenStores.triage(queue: queue, cards: try shown.map(Fixtures.card)) { $0.index = 3 }
        assertVariants { Harness(store: store) }
    }
}
