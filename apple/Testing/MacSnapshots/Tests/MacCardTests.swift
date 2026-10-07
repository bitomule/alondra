import AlondraContract
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
        XCTAssertEqual(buttons.visible.map(\.resolvedRole), [.advance, .neutral])
        XCTAssertEqual(buttons.more.map(\.label), ["Write my own answer", "Request clarification"])
        XCTAssertTrue(buttons.more.contains(where: \.asksForClarification), "clarification lives behind More…")
    }

    func test_buttons_neverShowMoreThanThreeAndMoveTheOverflowIntoMore() throws {
        let base = try card("card-question-mac")
        let many = try (1...5).map { try action("a\($0)", role: .neutral) }
        let json = try JSONEncoder().encode(base)
        var object = try XCTUnwrap(JSONSerialization.jsonObject(with: json) as? [String: Any])
        object["actions"] = try many.map { try JSONSerialization.jsonObject(with: JSONEncoder().encode($0)) }
        let crowded = try JSONDecoder().decode(Card.self, from: JSONSerialization.data(withJSONObject: object))
        let buttons = MacCardButtons(crowded)
        XCTAssertEqual(buttons.visible.map(\.label), ["a1", "a2", "a3"])
        XCTAssertEqual(Array(buttons.more.prefix(2)).map(\.label), ["a4", "a5"])
        XCTAssertEqual(buttons.more.count, 2 + base.more.count)
    }

    func test_buttons_leaveOutWhatAnOptimisticSendAlreadyHid() throws {
        let question = try card("card-question-mac")
        let first = try XCTUnwrap(question.actions.first?.id)
        let buttons = MacCardButtons(question, excluding: [first])
        XCTAssertTrue(buttons.visible.isEmpty, "the options share one action id, so all of them go")
    }

    func test_role_isRedOnlyForADestructiveAction() throws {
        XCTAssertEqual(try action("a", role: nil, style: .primary).resolvedRole, .advance)
        XCTAssertEqual(try action("a", role: nil, style: .destructive).resolvedRole, .risk)
        XCTAssertEqual(try action("a", role: nil, style: .secondary).resolvedRole, .neutral)
        XCTAssertEqual(try action("a", role: .risk, style: .primary).resolvedRole, .advance, "a risky role alone is not red")
        XCTAssertEqual(try action("a", role: .risk, style: .secondary).resolvedRole, .neutral)
    }

    func test_buttons_neverPaintAnActionTheServerMarksUnusable() throws {
        let base = try card("card-pr-mac")
        let json = try JSONEncoder().encode(base)
        var object = try XCTUnwrap(JSONSerialization.jsonObject(with: json) as? [String: Any])
        let usable = try action("usable", role: .advance, style: .primary)
        let unusable = try action("unusable", role: .advance, style: .primary, enabled: false)
        object["actions"] = try [unusable, usable].map { try JSONSerialization.jsonObject(with: JSONEncoder().encode($0)) }
        object["more"] = try [action("greyed", role: .neutral, enabled: false)].map { try JSONSerialization.jsonObject(with: JSONEncoder().encode($0)) }
        let card = try JSONDecoder().decode(Card.self, from: JSONSerialization.data(withJSONObject: object))
        let buttons = MacCardButtons(card)
        XCTAssertEqual(buttons.visible.map(\.label), ["usable"])
        XCTAssertTrue(buttons.more.isEmpty)
    }

    func test_pullRequest_advancesWithMergeAndHoldsTheRestBehindMore() throws {
        let review = try card("card-pr-mac")
        let buttons = MacCardButtons(review)
        XCTAssertEqual(buttons.visible.map(\.resolvedRole), [.advance, .neutral])
        XCTAssertEqual(buttons.visible.first?.enabled, true, "a card in Waiting for You offers a merge that works")
        XCTAssertTrue(review.blockedBy.isEmpty)
        XCTAssertTrue(buttons.more.contains { $0.link != nil }, "Open on GitHub is a link in More…")
    }

    func test_everyCardAlwaysCanBeRepliedTo() throws {
        for name in ["card-question-mac", "card-pr-mac", "card-information-mac"] {
            XCTAssertNotNil(try card(name).reply, name)
        }
    }
}
