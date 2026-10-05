import AlondraContract
import Foundation
import XCTest

final class QuestionStepsTests: XCTestCase {
    private func button(_ label: String, option: String? = nil, input: String = #"{"kind":"none"}"#, description: String? = nil) -> String {
        let optionField = option.map { #","option_id":"\#($0)""# } ?? ""
        let descriptionField = description.map { #","description":"\#($0)""# } ?? ""
        return #"{"id":"ans:card-1@3","label":"\#(label)","style":"secondary","presentation":"button","input":\#(input),"attachments":false,"enabled":true\#(optionField)\#(descriptionField)}"#
    }

    private func makeCard(steps: Int = 3) throws -> Card {
        let write = button("Write my own answer", input: #"{"kind":"text","placeholder":"Answer","required":true,"multiline":true}"#)
        let first = #"{"need":"How do the devices reach the Mac?","actions":[\#(button("★ LAN and Tailscale", option: "o1", description: "Works away too")),\#(button("LAN only", option: "o0"))],"write":\#(write)}"#
        let second = #"{"need":"Which port?","why":"The old one is taken","actions":[\#(button("7777", option: "o0")),\#(button("8080", option: "o1"))],"write":\#(write)}"#
        let third = #"{"need":"Anything else?","actions":[],"write":\#(write)}"#
        let all = [first, second, third].prefix(steps).joined(separator: ",")
        let json = #"{"id":"card-1","icon":"question","title":"How do the devices reach the Mac?","detail":"included","kind":"decide","need":"How do the devices reach the Mac?","steps":[\#(all)]}"#
        return try ContractCodec.decode(Card.self, from: Data(json.utf8))
    }

    func testACardDecodesItsStepsAndAnOlderCardHasNone() throws {
        let card = try makeCard()
        XCTAssertEqual(card.steps.map(\.need), ["How do the devices reach the Mac?", "Which port?", "Anything else?"])
        XCTAssertEqual(card.steps[0].actions.compactMap(\.optionId), ["o1", "o0"])
        XCTAssertEqual(card.steps[0].actions.first?.description, "Works away too")
        XCTAssertEqual(card.steps[1].why, "The old one is taken")
        XCTAssertNotNil(card.steps[2].write)
        XCTAssertTrue(card.actions.isEmpty)
        let plain = try ContractCodec.decode(Card.self, from: Data(#"{"id":"c","icon":"question","title":"T","detail":"included"}"#.utf8))
        XCTAssertTrue(plain.steps.isEmpty)
        XCTAssertNil(QuestionSteps().shown(in: plain))
    }

    func testTheQuestionShownAdvancesWithEachAnswerAndTheLastSendsEveryChoiceTogether() throws {
        let card = try makeCard()
        var steps = QuestionSteps()
        XCTAssertEqual(steps.index(in: card), 0)
        XCTAssertFalse(steps.canGoBack)
        XCTAssertNil(steps.choose(card.steps[0].actions[0], in: card), "the first answer sends nothing")
        XCTAssertEqual(steps.shown(in: card)?.need, "Which port?")
        XCTAssertTrue(steps.canGoBack)
        XCTAssertNil(steps.choose(card.steps[1].actions[1], in: card))
        XCTAssertEqual(steps.index(in: card), 2)
        let fields = try XCTUnwrap(steps.choose(try XCTUnwrap(card.steps[2].write), text: "  Mind the docs  ", in: card))
        XCTAssertEqual(fields["answers"], JSONValue.array([
            .object(["option_id": .string("o1"), "text": .null]),
            .object(["option_id": .string("o1"), "text": .null]),
            .object(["option_id": .null, "text": .string("Mind the docs")]),
        ]))
    }

    func testGoingBackForgetsTheLastAnswerOnly() throws {
        let card = try makeCard()
        var steps = QuestionSteps()
        _ = steps.choose(card.steps[0].actions[0], in: card)
        _ = steps.choose(card.steps[1].actions[0], in: card)
        steps.back()
        XCTAssertEqual(steps.answers, [QuestionAnswer(optionID: "o1", text: nil)])
        XCTAssertEqual(steps.shown(in: card)?.need, "Which port?")
        steps.back()
        steps.back()
        XCTAssertTrue(steps.answers.isEmpty)
    }

    func testAnAnswerNeedsAnOptionOfTheQuestionShownOrWords() throws {
        let card = try makeCard()
        var steps = QuestionSteps()
        XCTAssertNil(steps.choose(card.steps[1].actions[0], in: card), "an option of another question is not an answer")
        XCTAssertTrue(steps.answers.isEmpty)
        let write = try XCTUnwrap(card.steps[0].write)
        XCTAssertNil(steps.choose(write, text: "   ", in: card), "words are required to answer in words")
        XCTAssertTrue(steps.answers.isEmpty)
        XCTAssertNil(steps.choose(write, text: "Both", in: card))
        XCTAssertEqual(steps.answers, [QuestionAnswer(optionID: nil, text: "Both")])
    }

    func testTheCardShowsFourButtonsOfTheQuestionAndKeepsTheRestAndTheWayToWriteUnderMore() throws {
        let card = try makeCard()
        let steps = QuestionSteps()
        XCTAssertEqual(steps.buttons(in: card, limit: 4).map(\.label), ["★ LAN and Tailscale", "LAN only"])
        XCTAssertEqual(steps.menu(in: card, limit: 4).map(\.label), ["Write my own answer"])
        XCTAssertEqual(steps.buttons(in: card, limit: 1).map(\.label), ["★ LAN and Tailscale"])
        XCTAssertEqual(steps.menu(in: card, limit: 1).map(\.label), ["LAN only", "Write my own answer"])
    }
}
