import AlondraContract
import AlondraFeatures
import AlondraMacWorkspace
import XCTest

final class MacCardTests: XCTestCase {
    private func card(_ name: String) throws -> Card {
        let environment = ProcessInfo.processInfo.environment
        let root = URL(fileURLWithPath: try XCTUnwrap(environment["TEST_SRCDIR"]))
            .appendingPathComponent(environment["TEST_WORKSPACE"] ?? "_main")
        let document = try ContractCodec.decode(
            ViewDocument.self, from: Data(contentsOf: root.appendingPathComponent("fixtures/views/cards/\(name).json")))
        return try XCTUnwrap(document.sections.flatMap(\.items).compactMap { item -> Card? in
            if case let .card(card) = item { card } else { nil }
        }.first)
    }

    private func action(_ id: String, role: ActionRole?, style: Style = .secondary, enabled: Bool = true) throws -> AlondraContract.Action {
        let roleField = role.map { #""role":"\#($0.rawValue)","# } ?? ""
        let json = #"{"id":"\#(id)","label":"\#(id)","style":"\#(style.rawValue)",\#(roleField)"presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":\#(enabled)}"#
        return try JSONDecoder().decode(AlondraContract.Action.self, from: Data(json.utf8))
    }

    func test_buttons_showTheServerOrderAndKeepTheRestForMore() throws {
        let question = try card("card-question-mac")
        let buttons = MacCardButtons(question)
        XCTAssertEqual(buttons.visible.map(\.optionId), ["clear", "warm"])
        XCTAssertEqual(buttons.visible.map(\.paintedRole), [.advance, .neutral])
        XCTAssertTrue(buttons.more.isEmpty)
    }

    func test_actionBar_showsThreeAndMovesTheOverflowIntoMore() throws {
        let many = try (1...5).map { try action("a\($0)", role: .neutral) }
        let buttons = MacCardButtons(bar: many)
        XCTAssertEqual(buttons.visible.map(\.label), ["a1", "a2", "a3"])
        XCTAssertEqual(buttons.more.map(\.label), ["a4", "a5"])
    }

    func test_buttons_leaveOutWhatAnOptimisticSendAlreadyHid() throws {
        let question = try card("card-question-mac")
        let first = try XCTUnwrap(question.actions.first?.id)
        let buttons = MacCardButtons(question, excluding: [first])
        XCTAssertTrue(buttons.visible.isEmpty, "the options share one action id, so all of them go")
    }

    func test_role_isTheOneTheServerSends() throws {
        XCTAssertEqual(try action("a", role: .advance, style: .primary).paintedRole, .advance)
        XCTAssertEqual(try action("a", role: .risk, style: .destructive).paintedRole, .risk)
        XCTAssertEqual(try action("a", role: nil, style: .primary).paintedRole, .neutral, "an older daemon's unpainted button reads plain")
    }

    func test_pullRequest_advancesWithMergeAndHoldsTheRestBehindMore() throws {
        let review = try card("card-pr-mac")
        let buttons = MacCardButtons(review)
        XCTAssertEqual(buttons.visible.map(\.paintedRole), [.advance])
        XCTAssertEqual(buttons.visible.first?.enabled, true, "a card in Waiting for You offers a merge that works")
        XCTAssertTrue(review.blockedBy.isEmpty)
        XCTAssertEqual(buttons.more.map(\.label), ["Close without merging", "Open on GitHub"])
        XCTAssertTrue(buttons.more.contains { $0.link != nil }, "Open on GitHub is a link in More…")
        XCTAssertEqual(review.reply?.label, "Request changes")
    }

    func test_everyCardAlwaysCanBeRepliedTo() throws {
        for name in ["card-question-mac", "card-pr-mac", "card-information-mac"] {
            XCTAssertNotNil(try card(name).reply, name)
        }
    }
}
