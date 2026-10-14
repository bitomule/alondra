import AlondraContract
import AlondraFeatures
import ComposableArchitecture
import SwiftUI
import XCTest

final class ViewsUITests: XCTestCase {
    func testSidebarNodeDotsKeepTheirReleaseColors() throws {
        func tone(_ wire: String) throws -> Tone {
            try ContractCodec.decode(Tone.self, from: Data("\"\(wire)\"".utf8))
        }
        XCTAssertEqual(SidebarTint.node(try tone("accent")), .orange)
        XCTAssertEqual(SidebarTint.node(try tone("your_turn")), .pink)
        XCTAssertEqual(SidebarTint.node(try tone("neutral")), .gray)
        XCTAssertEqual(SidebarTint.task(try tone("your_turn")), .pink)
    }

    func testGoldenComponentDocumentsCanBeComposedBySharedRenderers() throws {
        let fixtureRoot = URL(fileURLWithPath: try XCTUnwrap(ProcessInfo.processInfo.environment["TEST_SRCDIR"]))
            .appendingPathComponent(ProcessInfo.processInfo.environment["TEST_WORKSPACE"] ?? "_main")
            .appendingPathComponent("fixtures/views/cards")

        for name in ["home-ios", "card-question", "card-pr", "card-delivery"] {
            let data = try Data(contentsOf: fixtureRoot.appendingPathComponent("\(name).json"))
            let document = try ContractCodec.decode(ViewDocument.self, from: data)

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
        let action = try ContractCodec.decode(AlondraContract.Action.self, from: bytes)
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

    func testSendStaysOffUntilTheActionHasWhatItAsksFor() throws {
        let required = try ContractCodec.decode(AlondraContract.Action.self, from: Data(#"{"id":"ask_pr:pr-1@1","label":"Ask about the PR","style":"secondary","presentation":"button","input":{"kind":"text","placeholder":"Your question","required":true,"multiline":true},"attachments":true,"enabled":true}"#.utf8))
        var state = ActionFeature.State(dataEpoch: "epoch", client: .ios, lang: "en")
        state.action = required
        XCTAssertFalse(state.canSubmit)
        state.text = "   \n"
        XCTAssertFalse(state.canSubmit, "whitespace is not a question")
        state.comments = [ActionComment(anchor: "a.swift:3", text: "Why?")]
        XCTAssertTrue(state.canSubmit, "line comments stand in for the text")
        state.comments = []
        state.text = "Why the new picker?"
        XCTAssertTrue(state.canSubmit)
        state.inFlight = true
        XCTAssertFalse(state.canSubmit, "one send at a time")

        let options = try ContractCodec.decode(AlondraContract.Action.self, from: Data(#"{"id":"ans:q-1@1","label":"Answer","style":"primary","presentation":"options","options":[{"id":"yes","label":"Yes","style":"primary","input":{"kind":"none"}}],"input":{"kind":"none"},"attachments":false,"enabled":true}"#.utf8))
        var choice = ActionFeature.State(dataEpoch: "epoch", client: .ios, lang: "en")
        choice.action = options
        XCTAssertFalse(choice.canSubmit)
        choice.selectedOptionID = "yes"
        XCTAssertTrue(choice.canSubmit)
    }

    @MainActor
    func testQuestionAcceptsFreeTextWithoutChoosingAnOption() async throws {
        let bytes = #"{"id":"ans:q-1@1","label":"Answer","style":"primary","presentation":"options","options":[{"id":"yes","label":"Yes","style":"primary","input":{"kind":"none"}}],"input":{"kind":"text","placeholder":"Answer in your own words…","required":false,"multiline":true},"attachments":false,"enabled":true}"#.data(using: .utf8)!
        let action = try ContractCodec.decode(AlondraContract.Action.self, from: bytes)
        let reply = try ContractCodec.decode(ActionResponse.self, from: #"{"protocol":1,"outcome":"applied","stamp":"Answered"}"#.data(using: .utf8)!)
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
        let request = ActionRequest(commandId: "5f0c2d9e-8d0b-4c71-9b0e-2b6f5d8f1a10",
                                    dataEpoch: "epoch", client: .ios, lang: "en", actionId: "ans:q-1@1",
                                    optionId: "", input: ActionInput(text: "My answer"))
        await store.send(.submit) {
            $0.inFlight = true
            $0.pending = request
            $0.sends = 1
        }
        await store.receive(\.sending, request)
        await store.receive(\.prepared) { $0.stamp = "Sent" }
        await store.receive(\.sent, request)
        await store.receive(\.settled)
        await store.receive(\.answered)
        await store.receive(\.response) {
            $0.inFlight = false
            $0.pending = nil
            $0.stamp = "Answered"
        }
        await store.receive(\.delegate.completed, "Action completed")
        await store.finish()
    }

    @MainActor
    func testTheScreenIsToldHowTheActionEndedExceptWhenTheDaemonAsksForAConfirmation() async throws {
        let action = try ContractCodec.decode(AlondraContract.Action.self, from: #"{"id":"close:n-1@1","label":"Close Node","style":"destructive","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":true}"#.data(using: .utf8)!)
        func outcome(_ json: String) throws -> ActionResponse {
            try ContractCodec.decode(ActionResponse.self, from: Data(json.utf8))
        }
        let store = TestStore(initialState: ActionFeature.State(dataEpoch: "epoch", client: .ios, lang: "en")) {
            ActionFeature()
        }
        store.exhaustivity = .off
        await store.send(.present(action))
        await store.send(.response(try outcome(#"{"protocol":1,"outcome":"rejected","message":"It is working"}"#)))
        await store.receive(\.delegate.failed, "It is working")
        await store.send(.response(try outcome(#"{"protocol":1,"outcome":"no_longer_available"}"#)))
        await store.receive(\.delegate.failed, "Action failed")
        await store.send(.failed("Couldn’t send. Try again."))
        await store.receive(\.delegate.failed, "Couldn’t send. Try again.")
        await store.send(.response(try outcome(#"{"protocol":1,"outcome":"changed","message":"The question changed"}"#)))
        await store.finish()
    }
}

final class ScheduleEditorRulesTests: XCTestCase {
    private func form() throws -> WireForm {
        let root = URL(fileURLWithPath: try XCTUnwrap(ProcessInfo.processInfo.environment["TEST_SRCDIR"]))
            .appendingPathComponent(ProcessInfo.processInfo.environment["TEST_WORKSPACE"] ?? "_main")
            .appendingPathComponent("fixtures/views/cards/schedule-new-ios.json")
        let document = try ContractCodec.decode(ViewDocument.self, from: Data(contentsOf: root))
        return try XCTUnwrap(document.sections.lazy.flatMap(\.items).compactMap { item -> WireForm? in
            if case let .form(form) = item { form } else { nil }
        }.first)
    }

    func testDailyScheduleShowsTimeButNotStartDate() throws {
        let ids = ScheduleEditorRules.visibleTimingFieldIDs(in: try form(), values: ["repeat": .string("daily")])
        XCTAssertEqual(ids, ["repeat", "time", "active"])
    }

    func testWeeklyScheduleAddsTheWeekday() throws {
        let ids = ScheduleEditorRules.visibleTimingFieldIDs(in: try form(), values: ["repeat": .string("weekly")])
        XCTAssertEqual(ids, ["repeat", "weekday", "time", "active"])
    }

    func testCustomScheduleShowsIntervalAndStartDateButNotTime() throws {
        let ids = ScheduleEditorRules.visibleTimingFieldIDs(in: try form(), values: ["repeat": .string("every_days")])
        XCTAssertEqual(ids, ["repeat", "interval", "start_at", "active"])
    }

    func testOnceScheduleShowsOnlyTheStartDate() throws {
        let ids = ScheduleEditorRules.visibleTimingFieldIDs(in: try form(), values: ["repeat": .string("once")])
        XCTAssertEqual(ids, ["repeat", "start_at", "active"])
    }

    func testRepeatModesCollapseTheCustomUnits() {
        XCTAssertEqual(ScheduleEditorRules.repeatMode("every_months"), "custom")
        XCTAssertEqual(ScheduleEditorRules.repeatMode("weekly"), "weekly")
        XCTAssertEqual(ScheduleEditorRules.repeatValue(forMode: "custom"), "every_hours")
        XCTAssertEqual(ScheduleEditorRules.repeatValue(forMode: "once"), "once")
    }
}
