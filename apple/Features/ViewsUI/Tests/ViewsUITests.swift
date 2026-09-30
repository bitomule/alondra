import AlondraViews
import AlondraViewsUI
import ComposableArchitecture
import XCTest

final class ViewsUITests: XCTestCase {
    func testGoldenComponentDocumentsCanBeComposedBySharedRenderers() throws {
        let fixtureRoot = URL(fileURLWithPath: try XCTUnwrap(ProcessInfo.processInfo.environment["TEST_SRCDIR"]))
            .appendingPathComponent(ProcessInfo.processInfo.environment["TEST_WORKSPACE"] ?? "_main")
            .appendingPathComponent("fixtures/views")

        for name in ["home-ios", "card-question", "card-pr", "card-delivery"] {
            let data = try Data(contentsOf: fixtureRoot.appendingPathComponent("\(name).json"))
            let document = try ViewsCodec.decode(ViewDocument.self, from: data)

            for section in document.sections {
                _ = SectionView(section: section)
                for item in section.items {
                    _ = ViewItemRenderer(item: item)
                }
            }
        }
    }

    @MainActor
    func testActionRequiresTextAndConfirmationBeforeSending() async throws {
        let bytes = #"{"id":"reply:i-1@1","label":"Reply","style":"primary","presentation":"button","input":{"kind":"text","required":true,"multiline":true},"attachments":false,"confirm":{"title":"Send reply","message":"Send this reply?","button":"Send"},"enabled":true}"#.data(using: .utf8)!
        let action = try ViewsCodec.decode(AlondraViews.Action.self, from: bytes)
        let store = TestStore(initialState: ActionFeature.State(dataEpoch: "epoch", client: .ios, lang: "en")) {
            ActionFeature()
        }
        await store.send(.present(action)) { $0.action = action }
        await store.send(.submit) { $0.error = "Enter a response" }
        await store.send(.textChanged("Please explain the tradeoff")) {
            $0.text = "Please explain the tradeoff"
            $0.error = nil
        }
        await store.send(.submit) { $0.confirmation = action.confirm }
        await store.send(.cancelConfirmation) { $0.confirmation = nil }
    }
}
