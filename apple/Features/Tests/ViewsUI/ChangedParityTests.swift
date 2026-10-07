import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import Foundation
import XCTest

/// Design §7.4: the same "changed" answer from the server reads and behaves the same on the iPhone
/// and on the Mac. "Send anyway" takes the card out at once on both, as any other answer does.
final class ChangedParityTests: XCTestCase {
    private static let changed = #"{"protocol":1,"outcome":"changed","result":{"kind":"changed","title":"PR #12 has new commits","body":"You reviewed an older version.","next":{"id":"ans:next@2","label":"Send anyway","style":"primary","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":true}}}"#

    /// The first answer comes back "changed"; the one sent anyway never gets an answer, so the card
    /// stays wherever the client put it when it was sent.
    private final class Daemon: @unchecked Sendable {
        private let lock = NSLock()
        private var sent: [ActionRequest] = []
        var requests: [ActionRequest] { lock.withLock { sent } }

        func perform(_ request: ActionRequest) async throws -> ActionResponse {
            let call = lock.withLock { sent.append(request); return sent.count }
            if call > 1 { try await Task.sleep(for: .seconds(3600)) }
            return try ContractCodec.decode(ActionResponse.self, from: Data(ChangedParityTests.changed.utf8))
        }
    }

    @MainActor
    func testSendAnywayTakesTheCardOutAtOnceOnPhoneAndMac() async throws {
        let phone = try await phoneHidesAfterSendingAnyway()
        let mac = try await macHidesAfterSendingAnyway()
        XCTAssertEqual(phone.question, mac.question)
        XCTAssertEqual(phone.sentAnyway, ["ans:next@2"])
        XCTAssertEqual(mac.sentAnyway, ["ans:next@2"])
        XCTAssertTrue(phone.hidden, "iPhone: the card left when Send anyway was pressed")
        XCTAssertTrue(mac.hidden, "Mac: the card left when Send anyway was pressed")
    }

    private static let answer = #"{"id":"ans:x@1","label":"Answer","style":"primary","presentation":"button","input":{"kind":"text","required":true,"multiline":true},"attachments":false,"enabled":true}"#

    /// What is on screen once Cancel closed the "changed" question.
    private struct AfterCancel: Equatable {
        let question: String?
        let questionLeft: Bool
        let answering: String?
        let text: String
        let sheetShown: Bool
    }

    @MainActor
    func testCancelOnChangedClosesOnlyTheQuestionAndGoesBackToTheAnswerOnPhoneAndMac() async throws {
        let action = try ContractCodec.decode(AlondraContract.Action.self, from: Data(Self.answer.utf8))
        let reply = try ContractCodec.decode(ActionResponse.self, from: Data(Self.changed.utf8))

        let phone = TestStore(initialState: ViewsScreenFeature.State(key: ViewKey(rawValue: "card:a"), owner: "changed-parity")) {
            ViewsScreenFeature()
        }
        phone.exhaustivity = .off
        await phone.send(.action(.present(action)))
        await phone.send(.action(.textChanged("Use the compact one")))
        await phone.send(.action(.response(reply)))
        let phoneQuestion = phone.state.action.changed?.title
        XCTAssertFalse(phone.state.actionSheetPresented, "iPhone: the question is not left under the sheet")
        await phone.send(.action(.dismissChanged))
        await phone.skipReceivedActions(strict: false)

        var state = MacPresentationFeature.State()
        state.sidebarDocument = try Self.sidebar(waiting: ["a", "b"])
        state.selection = .viewKey("card:a")
        state.selectedViewKey = "card:a"
        state.detailDocument = try Self.document(key: "card:a")
        let mac = TestStore(initialState: state) { MacPresentationFeature() } withDependencies: {
            $0.continuousClock = TestClock()
        }
        mac.exhaustivity = .off
        await mac.send(.beginViewAction(action))
        await mac.send(.action(.textChanged("Use the compact one")))
        await mac.send(.action(.response(reply)))
        let macQuestion = mac.state.actionFeature.changed?.title
        await mac.send(.action(.dismissChanged))
        await mac.skipReceivedActions(strict: false)

        let onPhone = AfterCancel(question: phoneQuestion, questionLeft: phone.state.action.changed == nil,
                                  answering: phone.state.action.action?.id, text: phone.state.action.text,
                                  sheetShown: phone.state.actionSheetPresented)
        let onMac = AfterCancel(question: macQuestion, questionLeft: mac.state.actionFeature.changed == nil,
                                answering: mac.state.actionFeature.action?.id, text: mac.state.actionFeature.text,
                                sheetShown: mac.state.actionPresented)
        XCTAssertEqual(onPhone, onMac)
        XCTAssertEqual(onPhone, AfterCancel(question: "PR #12 has new commits", questionLeft: true,
                                            answering: "ans:x@1", text: "Use the compact one", sheetShown: true))
    }

    private struct Seen: Equatable {
        let question: String?
        let sentAnyway: [String]
        let hidden: Bool
    }

    @MainActor
    private func phoneHidesAfterSendingAnyway() async throws -> Seen {
        let id = Fixtures.questionID
        let queue = [id, Fixtures.informationID]
        let feed = FeedStub([try Fixtures.home(queue: queue)] + (try queue.map(Fixtures.card)))
        let daemon = Daemon()
        let store = TestStore(initialState: TriageFeature.State()) { TriageFeature() } withDependencies: {
            $0.continuousClock = TestClock()
            $0.viewsClient.observe = feed.observe
            $0.viewsClient.perform = { try await daemon.perform($0) }
        }
        store.exhaustivity = .off
        await store.send(.task)
        await store.skipReceivedActions()

        let action = try XCTUnwrap(store.state.cards[id: id]?.buttons.first)
        await store.send(.cards(.element(id: id, action: .actionTapped(action))))
        await store.skipReceivedActions()
        let question = store.state.cards[id: id]?.actions.changed?.title
        XCTAssertFalse(store.state.optimistic.hides(id), "a changed answer brings the card back")

        await store.send(.cards(.element(id: id, action: .actions(.sendChangedAction))))
        await store.skipReceivedActions(strict: false)
        let seen = Seen(question: question, sentAnyway: daemon.requests.dropFirst().map(\.actionId), hidden: store.state.optimistic.hides(id))
        await store.skipInFlightEffects(strict: false)
        return seen
    }

    @MainActor
    private func macHidesAfterSendingAnyway() async throws -> Seen {
        var state = MacPresentationFeature.State()
        state.sidebarDocument = try Self.sidebar(waiting: ["a", "b"])
        state.selection = .viewKey("card:a")
        state.selectedViewKey = "card:a"
        state.detailDocument = try Self.document(key: "card:a")
        let daemon = Daemon()
        let store = TestStore(initialState: state) { MacPresentationFeature() } withDependencies: {
            $0.continuousClock = TestClock()
            $0.viewsClient.openScreen = { _, _ in }
            $0.viewsClient.closeScreen = { _ in }
            $0.viewsClient.load = { key, _ in
                key.rawValue == "sidebar" ? .document(try Self.sidebar(waiting: ["a", "b"])) : .document(try Self.document(key: key.rawValue))
            }
            $0.viewsClient.perform = { try await daemon.perform($0) }
        }
        store.exhaustivity = .off

        let button = try ContractCodec.decode(AlondraContract.Action.self, from: Data(
            #"{"id":"ans:x@1","label":"Ship it","style":"primary","option_id":"clear","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":true}"#.utf8))
        await store.send(.beginViewAction(button))
        await store.skipReceivedActions()
        let question = store.state.actionFeature.changed?.title
        XCTAssertFalse(store.state.optimistic.hides("a"), "a changed answer brings the card back")
        XCTAssertEqual(store.state.selection, .viewKey("card:a"))

        await store.send(.action(.sendChangedAction))
        await store.skipReceivedActions(strict: false)
        let seen = Seen(question: question, sentAnyway: daemon.requests.dropFirst().map(\.actionId), hidden: store.state.optimistic.hides("a"))
        XCTAssertEqual(store.state.selection, .viewKey("card:b"))
        await store.skipInFlightEffects(strict: false)
        return seen
    }

    private static func sidebar(waiting: [String]) throws -> ViewDocument {
        let rows = waiting.map {
            #"{"type":"row","id":"\#($0)","icon":"question","title":"Card \#($0)","depth":0,"struck":false,"link":{"kind":"view","key":"card:\#($0)"},"children":[]}"#
        }.joined(separator: ",")
        return try document(key: "sidebar", sections: #"[{"id":"waiting","collapsed":false,"items":[\#(rows)]}]"#)
    }

    private static func document(key: String, sections: String = "[]") throws -> ViewDocument {
        let status = #"{"version":"s1","summary":"Ready","badge":0,"counts":[],"gauges":[],"orchestrator":{"text":"Ready","tone":"success"}}"#
        let json = #"{"protocol":1,"view":"\#(key)","key":"\#(key)","client":"mac","version":"v1","cursor":"c1.abc","data_epoch":"epoch","status":\#(status),"sections":\#(sections),"notices":[]}"#
        return try ContractCodec.decode(ViewDocument.self, from: Data(json.utf8))
    }
}
