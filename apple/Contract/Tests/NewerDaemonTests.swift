import Foundation
import XCTest
import AlondraContract

/// A daemon newer than the app sends kinds of links, items, fields and buttons the app does not
/// know: only those elements change, and the rest of the view loads.
final class NewerDaemonTests: XCTestCase {
    private func newerCard() throws -> ViewDocument {
        let env = ProcessInfo.processInfo.environment
        let url = URL(fileURLWithPath: try XCTUnwrap(env["TEST_SRCDIR"]))
            .appendingPathComponent(env["TEST_WORKSPACE"] ?? "_main")
            .appendingPathComponent("fixtures/views/newer-daemon/card.json")
        return try ContractCodec.decode(ViewDocument.self, from: Data(contentsOf: url))
    }

    private func card(_ view: ViewDocument) throws -> Card {
        try XCTUnwrap(view.sections.first?.items.compactMap { item -> Card? in
            if case let .card(card) = item { card } else { nil }
        }.first)
    }

    func testAViewFromANewerDaemonLoadsWithEverythingTheAppKnows() throws {
        let view = try newerCard()
        let card = try card(view)

        XCTAssertEqual(card.title, "Sign in to the calendar: 1 task waits for it")
        XCTAssertEqual(card.actions.map(\.label), ["Sign in with a passkey", "Go on without the calendar", "Pick a time"])
        XCTAssertEqual(view.sections.map(\.id), ["card", "context"])
        XCTAssertEqual(view.sections[1].items.count, 4)
    }

    func testAButtonWithAnUnknownLinkOrPresentationShowsDisabledAskingToUpdate() throws {
        let actions = try card(try newerCard()).actions
        let passkey = actions[0], answer = actions[1], slider = actions[2]

        XCTAssertFalse(passkey.enabled)
        XCTAssertTrue(passkey.needsNewerApp)
        XCTAssertNil(passkey.link)
        XCTAssertFalse(slider.enabled)
        XCTAssertTrue(slider.needsNewerApp)
        XCTAssertEqual(slider.presentation, .button)
        XCTAssertTrue(answer.enabled)
        XCTAssertFalse(answer.needsNewerApp)
    }

    func testUnknownValuesDrawWithTheirPlainLook() throws {
        let view = try newerCard()
        let card = try card(view)

        XCTAssertNil(card.kind)
        XCTAssertEqual(card.icon, .notice)
        XCTAssertEqual(card.eyebrow?.tone, .neutral)
        XCTAssertEqual(card.detail, .reference)
        XCTAssertEqual(card.actions[1].style, .secondary)
        XCTAssertNil(card.actions[1].role)
        XCTAssertEqual(view.status.gauges.first?.level, .calm)
        XCTAssertEqual(view.status.orchestrator.tone, .neutral)
    }

    func testAnElementWithAnUnknownLinkStaysWithoutIt() throws {
        let view = try newerCard()
        let card = try card(view)
        guard case let .facts(facts) = view.sections[1].items[0] else { return XCTFail("facts") }

        XCTAssertEqual(card.context.first?.text, "Plan the release week")
        XCTAssertNil(card.context.first?.link)
        XCTAssertEqual(card.link, .view(key: "card:calendar-sign-in-t-31"))
        XCTAssertEqual(facts.items.map(\.link), [.view(key: "task:t-31"), nil])
    }

    func testAnUnknownItemStaysInItsPlaceAsUnsupported() throws {
        let items = try newerCard().sections[1].items

        XCTAssertEqual(items[1], .unsupported)
        guard case let .notice(notice) = items[2] else { return XCTFail("\(items[2])") }
        XCTAssertEqual(notice.text, "The calendar asks for a passkey once a week.")
    }

    func testAFormLeavesOutAnUnknownFieldAndCannotBeSentWhenItWasRequired() throws {
        guard case let .form(form) = try newerCard().sections[1].items[3] else { return XCTFail("form") }

        XCTAssertEqual(form.fields.map(\.id), ["note"])
        XCTAssertFalse(form.submit.enabled)
        XCTAssertTrue(form.submit.needsNewerApp)
    }

    func testAnOptionalUnknownFieldLeavesTheFormUsable() throws {
        let json = #"{"id":"f","fields":[{"id":"a","label":"A","kind":{"kind":"globe"},"value":null,"required":false,"reload_on_change":false}],"submit":{"label":"Save","style":"primary","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":true}}"#
        let form = try ContractCodec.decode(Form.self, from: Data(json.utf8))

        XCTAssertTrue(form.fields.isEmpty)
        XCTAssertTrue(form.submit.enabled)
    }

    func testUnknownTrailingCommentsAndOptionsAreLeftOut() throws {
        let row = #"{"id":"r","icon":"task","title":"Row","trailing":{"kind":"sparkline","points":[1,2]},"depth":0,"struck":false}"#
        XCTAssertNil(try ContractCodec.decode(Row.self, from: Data(row.utf8)).trailing)

        let action = #"{"id":"a","label":"Answer","style":"primary","presentation":"options","input":{"kind":"none"},"attachments":false,"enabled":true,"comments":{"kind":"pixels"},"options":[{"id":"x","label":"X","style":"primary","input":{"kind":"none"}},{"id":"y","label":"Y","style":"primary","input":{"kind":"voice"}}]}"#
        let decoded = try ContractCodec.decode(AlondraContract.Action.self, from: Data(action.utf8))
        XCTAssertTrue(decoded.enabled)
        XCTAssertNil(decoded.comments)
        XCTAssertEqual(decoded.options.map(\.id), ["x"])
    }

    func testALabelThatCountsTimeReadsAsItsTextOnAnAppThatDoesNotCount() throws {
        let row = #"{"id":"t","icon":"task","title":"Task","badges":[{"text":"Working","tone":"info"},{"text":"running since 08:05","tone":"neutral","elapsed":{"since":"2026-09-30T08:05:00.000+02:00"}}],"depth":0,"struck":false}"#
        XCTAssertEqual(try ContractCodec.decode(Row.self, from: Data(row.utf8)).badges.map(\.text), ["Working", "running since 08:05"])
    }

    func testAnOutcomeFromANewerDaemonReadsAsChanged() throws {
        let reply = try ContractCodec.decode(ActionResponse.self, from: Data(#"{"protocol":1,"outcome":"queued"}"#.utf8))
        XCTAssertEqual(reply.outcome, .changed)
    }

    func testAKnownKindWithABrokenPayloadStillFails() throws {
        let missingKey = #"{"id":"r","icon":"task","title":"Row","link":{"kind":"view"},"depth":0,"struck":false}"#
        XCTAssertThrowsError(try ContractCodec.decode(Row.self, from: Data(missingKey.utf8)))

        let missingPresentation = #"{"label":"Go","style":"primary","input":{"kind":"none"},"attachments":false,"enabled":true}"#
        XCTAssertThrowsError(try ContractCodec.decode(AlondraContract.Action.self, from: Data(missingPresentation.utf8)))
    }

    func testWhatANewerDaemonSentStaysTheSameOnceCachedAndReadAgain() throws {
        let view = try newerCard()
        let again = try ContractCodec.decode(ViewDocument.self, from: ContractCodec.encode(view))

        XCTAssertEqual(again, view)
        XCTAssertTrue(try card(again).actions[0].needsNewerApp)
    }
    func testFormIntentMetadataAcceptsEditedUneditedAndLegacyDocuments() throws {
        let document = #"{"id":"f","fields":[],"submit":{"label":"Save","style":"primary","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":true}}"#
        let legacy = try ContractCodec.decode(Form.self, from: Data(document.utf8))
        XCTAssertNil(legacy.substitutionWasEdited)
        XCTAssertNil(legacy.initialValues["substitution_was_edited"])
        for edited in [true, false] {
            let current = document.replacingOccurrences(of: #""id":"f""#, with: #""id":"f","substitution_was_edited":\#(edited)"#)
            let form = try ContractCodec.decode(Form.self, from: Data(current.utf8))
            XCTAssertEqual(form.substitutionWasEdited, edited)
            XCTAssertEqual(form.initialValues["substitution_was_edited"], .bool(edited))
        }
    }

}
