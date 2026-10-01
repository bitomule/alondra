import AlondraViews
import AlondraViewsUI
import ComposableArchitecture
import SwiftUI
import XCTest

final class ViewsUITests: XCTestCase {
    func testSidebarNodeDotsKeepTheirReleaseColors() throws {
        func tone(_ wire: String) throws -> Tone {
            try ViewsCodec.decode(Tone.self, from: Data("\"\(wire)\"".utf8))
        }
        XCTAssertEqual(SidebarTint.node(try tone("accent")), .orange)
        XCTAssertEqual(SidebarTint.node(try tone("your_turn")), .pink)
        XCTAssertEqual(SidebarTint.node(try tone("unclear")), .purple)
        XCTAssertEqual(SidebarTint.node(try tone("neutral")), .gray)
        XCTAssertEqual(SidebarTint.task(try tone("your_turn")), .pink)
        XCTAssertEqual(SidebarTint.task(try tone("unclear")), .purple)
    }

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

    @MainActor
    func testQuestionAcceptsFreeTextWithoutChoosingAnOption() async throws {
        let bytes = #"{"id":"ans:q-1@1","label":"Answer","style":"primary","presentation":"options","options":[{"id":"yes","label":"Yes","style":"primary","input":{"kind":"none"}}],"input":{"kind":"text","placeholder":"Answer in your own words…","required":false,"multiline":true},"attachments":false,"enabled":true}"#.data(using: .utf8)!
        let action = try ViewsCodec.decode(AlondraViews.Action.self, from: bytes)
        let reply = try ViewsCodec.decode(ActionResponse.self, from: #"{"protocol":1,"outcome":"applied","stamp":"Answered"}"#.data(using: .utf8)!)
        let store = TestStore(initialState: ActionFeature.State(dataEpoch: "epoch", client: .ios, lang: "en")) {
            ActionFeature()
        } withDependencies: {
            $0.uuid = .constant(UUID(uuidString: "5F0C2D9E-8D0B-4C71-9B0E-2B6F5D8F1A10")!)
            $0.viewsClient.perform = { request in
                XCTAssertEqual(request.optionId, "")
                XCTAssertEqual(request.input.text, "My answer")
                return reply
            }
        }
        await store.send(.present(action)) { $0.action = action }
        await store.send(.submit) { $0.error = "Choose an option" }
        await store.send(.textChanged("My answer")) {
            $0.text = "My answer"
            $0.error = nil
        }
        await store.send(.submit) {
            $0.inFlight = true
            $0.pending = ActionRequest(commandId: "5f0c2d9e-8d0b-4c71-9b0e-2b6f5d8f1a10",
                                           dataEpoch: "epoch", client: .ios, lang: "en", actionId: "ans:q-1@1",
                                           optionId: "", input: ActionInput(text: "My answer"))
        }
        await store.receive(\.response) {
            $0.inFlight = false
            $0.pending = nil
            $0.stamp = "Answered"
        }
    }
}
