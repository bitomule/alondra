import AlondraContract
import AlondraDesign
import AlondraFeatures
import AlondraiOSUI
import ComposableArchitecture
import SwiftUI
import XCTest

/// The review screen: a delivery, a document, a question, with and without comments, and while
/// it is still loading.
@MainActor
final class ReviewSnapshotTests: XCTestCase {
    override func setUp() async throws { ScreenStores.pinTime() }

    private struct Harness: View {
        let store: StoreOf<ReviewFeature>

        var body: some View {
            NavigationStack { ReviewView(store: store) }.frame(height: 852)
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

    // MARK: Loading

    func test_loading() {
        assertVariants {
            Harness(store: Store(initialState: ReviewFeature.State(key: .card("c-1"))) { ReviewFeature() })
        }
    }

    func test_noLongerAvailable() {
        var state = ReviewFeature.State(key: .card("c-1"))
        state.screen.phase = .missing
        assertVariants { Harness(store: ScreenStores.review(state: state)) }
    }

    // MARK: A delivery

    func test_delivery() throws {
        let store = ScreenStores.review(try Fixtures.liveDelivery())
        assertVariants { Harness(store: store) }
    }

    func test_delivery_es() throws {
        let store = ScreenStores.review(try Fixtures.liveDelivery())
        assertVariants(language: .spanish) { Harness(store: store) }
    }

    func test_deliveryWithComments() throws {
        let document = try Fixtures.liveDelivery()
        let store = ScreenStores.review(document) { state in
            let blocks = state.blocks.filter(\.isCommentable)
            for block in blocks.prefix(2) {
                state.comments[block.draftKey] = ActionComment(
                    anchor: block.draftKey, text: "Say which simulator", quote: block.plainText
                )
            }
            state.general = "Looks right overall."
        }
        assertVariants { Harness(store: store) }
    }

    func test_deliveryWithComments_es() throws {
        let store = ScreenStores.review(try Fixtures.liveDelivery()) { state in
            if let block = state.blocks.first(where: \.isCommentable) {
                state.comments[block.draftKey] = ActionComment(anchor: block.draftKey, text: "Indica el simulador", quote: block.plainText)
            }
        }
        assertVariants(language: .spanish) { Harness(store: store) }
    }

    // MARK: A document

    func test_documentFetching() throws {
        let store = ScreenStores.review(try Fixtures.documentRevision()) { state in
            state.contentDigest = "digest-1"
        }
        assertVariants { Harness(store: store) }
    }

    func test_document() throws {
        let store = ScreenStores.review(try Fixtures.documentRevision()) { state in
            state.contentDigest = "digest-1"
            state.content = plan
        }
        assertVariants { Harness(store: store) }
    }

    func test_documentThatCouldNotBeFetched() throws {
        let store = ScreenStores.review(try Fixtures.documentRevision()) { state in
            state.contentDigest = "digest-1"
            state.contentFailed = true
        }
        assertVariants { Harness(store: store) }
    }

    func test_plan() throws {
        let store = ScreenStores.review(try Fixtures.planCard())
        assertVariants { Harness(store: store) }
    }

    // MARK: Other cards read in full

    func test_question() throws {
        let store = ScreenStores.review(try Fixtures.card(Fixtures.questionID))
        assertVariants { Harness(store: store) }
    }

    func test_report() throws {
        let store = ScreenStores.review(try Fixtures.card(Fixtures.informationID))
        assertVariants { Harness(store: store) }
    }

    // MARK: Commenting

    func test_commentSheet() throws {
        let document = try Fixtures.liveDelivery()
        let store = ScreenStores.review(document) { state in
            let block = state.blocks.first(where: \.isCommentable)!
            state.composing = ReviewFeature.Composing(
                anchor: block.draftKey, quote: block.plainText, text: "Which simulator was it?"
            )
        }
        let composing = store.withState { $0.composing! }
        assertVariants { NavigationStack { CommentSheet(store: store, composing: composing) }.frame(height: 560) }
    }
}
