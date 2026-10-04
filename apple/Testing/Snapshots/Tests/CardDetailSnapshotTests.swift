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

    private struct Harness: View {
        let store: StoreOf<CardDetailFeature>

        var body: some View {
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

    private func fetchedPlan(edit: (inout CardDetailFeature.State) -> Void = { _ in }) throws
        -> StoreOf<CardDetailFeature> {
        ScreenStores.cardDetail(try Fixtures.planCard()) { state in
            state.contentDigest = "plan-digest"
            state.content = plan
            edit(&state)
        }
    }

    // MARK: Loading

    func test_loading() {
        assertVariants {
            Harness(store: Store(initialState: CardDetailFeature.State(key: .card("c-1"))) { CardDetailFeature() })
        }
    }

    func test_noLongerAvailable() {
        var state = CardDetailFeature.State(key: .card("c-1"))
        state.screen.phase = .missing
        assertVariants { Harness(store: ScreenStores.cardDetail(state: state)) }
    }

    // MARK: Each kind of card in full

    func test_decide() throws {
        let store = ScreenStores.cardDetail(try Fixtures.card(Fixtures.questionID))
        assertVariants { Harness(store: store) }
    }

    func test_decide_es() throws {
        let store = ScreenStores.cardDetail(try Fixtures.card(Fixtures.questionID))
        assertVariants(language: .spanish) { Harness(store: store) }
    }

    func test_decideWithItsHistory() throws {
        let store = ScreenStores.cardDetail(try Fixtures.card(Fixtures.questionID).withHistory())
        assertVariants { Harness(store: store) }
    }

    func test_authorize() throws {
        let store = ScreenStores.cardDetail(try Fixtures.authorizeCard())
        assertVariants { Harness(store: store) }
    }

    func test_problem() throws {
        let store = ScreenStores.cardDetail(try Fixtures.problemCard())
        assertVariants { Harness(store: store) }
    }

    func test_know() throws {
        let store = ScreenStores.cardDetail(try Fixtures.card(Fixtures.informationID))
        assertVariants { Harness(store: store) }
    }

    func test_pullRequestHeldBackByAnotherCard() throws {
        let store = ScreenStores.cardDetail(try Fixtures.card(Fixtures.pullRequestID))
        assertVariants { Harness(store: store) }
    }

    func test_pullRequestReadyToMerge() throws {
        let store = ScreenStores.cardDetail(try Fixtures.unblocked(try Fixtures.card(Fixtures.pullRequestID)))
        assertVariants { Harness(store: store) }
    }

    func test_pullRequestReadyToMerge_es() throws {
        let store = ScreenStores.cardDetail(try Fixtures.unblocked(try Fixtures.card(Fixtures.pullRequestID)))
        assertVariants(language: .spanish) { Harness(store: store) }
    }

    // MARK: A plan to comment on

    func test_plan() throws {
        let store = try fetchedPlan()
        assertVariants { Harness(store: store) }
    }

    func test_planWithComments() throws {
        let store = try fetchedPlan { state in
            for block in state.blocks.filter(\.isCommentable).prefix(2) {
                state.comments[block.draftKey] = ActionComment(
                    anchor: block.draftKey, text: "Say which simulator", quote: block.plainText
                )
            }
            state.reply = "Looks right overall."
        }
        assertVariants { Harness(store: store) }
    }

    func test_planWithComments_es() throws {
        let store = try fetchedPlan { state in
            if let block = state.blocks.first(where: \.isCommentable) {
                state.comments[block.draftKey] = ActionComment(
                    anchor: block.draftKey, text: "Indica el simulador", quote: block.plainText
                )
            }
        }
        assertVariants(language: .spanish) { Harness(store: store) }
    }

    func test_planFetching() throws {
        let store = ScreenStores.cardDetail(try Fixtures.planCard()) { $0.contentDigest = "plan-digest" }
        assertVariants { Harness(store: store) }
    }

    func test_planThatCouldNotBeFetched() throws {
        let store = ScreenStores.cardDetail(try Fixtures.planCard()) { state in
            state.contentDigest = "plan-digest"
            state.contentFailed = true
        }
        assertVariants { Harness(store: store) }
    }

    // MARK: A document opened by itself

    func test_documentFetching() throws {
        let store = ScreenStores.cardDetail(try Fixtures.documentRevision()) { state in
            state.contentDigest = "digest-1"
        }
        assertVariants { Harness(store: store) }
    }

    func test_document() throws {
        let store = ScreenStores.cardDetail(try Fixtures.documentRevision()) { state in
            state.contentDigest = "digest-1"
            state.content = plan
        }
        assertVariants { Harness(store: store) }
    }

    // MARK: Commenting

    func test_commentSheet() throws {
        let store = try fetchedPlan { state in
            let block = state.blocks.first(where: \.isCommentable)!
            state.composing = CardDetailFeature.Composing(
                anchor: block.draftKey, quote: block.plainText, text: "Which simulator was it?"
            )
        }
        let composing = store.withState { $0.composing! }
        assertVariants { NavigationStack { CommentSheet(store: store, composing: composing) }.frame(height: 560) }
    }
}
