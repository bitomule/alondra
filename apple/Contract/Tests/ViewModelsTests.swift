import Foundation
import XCTest
import AlondraContract

final class ViewModelsTests: XCTestCase {
    func testDecodesRustGoldenViews() throws {
        let env = ProcessInfo.processInfo.environment
        let root = URL(fileURLWithPath: try XCTUnwrap(env["TEST_SRCDIR"]))
            .appendingPathComponent(env["TEST_WORKSPACE"] ?? "_main")
            .appendingPathComponent("fixtures/views/cards")
        for name in ["home-ios", "home-ios-es", "home-tui", "card-question", "card-pr", "card-delivery"] {
            let data = try Data(contentsOf: root.appendingPathComponent("\(name).json"))
            let view = try ContractCodec.decode(ViewDocument.self, from: data)
            XCTAssertEqual(view.protocolVersion, 1, name)
            if name.hasPrefix("home-ios") {
                XCTAssertEqual(view.sections.first?.cardIds?.count, 5, name)
                XCTAssertEqual(view.sections.first?.items.count, 0, name)
            }
        }
    }

    func testDecodesTheCardAnatomyOfTheRedesignedGoldens() throws {
        let env = ProcessInfo.processInfo.environment
        let root = URL(fileURLWithPath: try XCTUnwrap(env["TEST_SRCDIR"]))
            .appendingPathComponent(env["TEST_WORKSPACE"] ?? "_main")
            .appendingPathComponent("fixtures/views/cards")
        let view = try ContractCodec.decode(ViewDocument.self, from: Data(contentsOf: root.appendingPathComponent("card-pr.json")))
        let card = try XCTUnwrap(view.sections.flatMap(\.items).compactMap { item -> Card? in
            if case let .card(card) = item { card } else { nil }
        }.first)
        XCTAssertEqual(card.kind, .review)
        XCTAssertEqual(card.need, "Review the beta candidate")
        XCTAssertEqual(card.title, "Add the compact schedule editor")
        XCTAssertEqual(card.contentRow?.kind, "diff")
        XCTAssertTrue(card.blockedBy.isEmpty, "a delivery held back is no card, so the golden review has nothing holding it")
        XCTAssertNotNil(card.reply)
        XCTAssertEqual(card.actions.first?.role, .advance)
        XCTAssertTrue(card.more.contains { $0.link != nil })
        let question = try ContractCodec.decode(ViewDocument.self, from: Data(contentsOf: root.appendingPathComponent("card-question.json")))
        let decide = try XCTUnwrap(question.sections.flatMap(\.items).compactMap { item -> Card? in
            if case let .card(card) = item { card } else { nil }
        }.first)
        XCTAssertEqual(decide.actions.compactMap(\.optionId), ["clear", "warm"])
        XCTAssertEqual(decide.actions.first?.description, "Use a short title and one explanatory sentence.")
    }

    func testFeedStatusKeepsEveryClaudeGaugeTheMacSends() throws {
        let json = #"{"version":"v66b2a1f4b10ae439","summary":"0 nodos trabajando","badge":0,"counts":[],"gauges":[{"id":"claude_week","label":"Claude semana","value":0.83,"text":"83%","detail":"El límite semanal se reinicia 2026-10-05T02:00:00.000+02:00","level":"calm"},{"id":"claude_today","label":"Claude hoy","value":0.0,"text":"0/6%","detail":"Gastado 0% de la cuota diaria del 6%","level":"calm"},{"id":"claude_five_hours","label":"Claude 5 h","value":0.0,"text":"0%","detail":"El límite de 5 horas se reinicia 2026-10-03T04:40:00.000+02:00","level":"calm"},{"id":"memory","label":"RAM","value":0.39,"text":"39%","level":"calm"},{"id":"cpu","label":"CPU","value":0.2365247917175293,"text":"24%","level":"calm"},{"id":"disk","label":"Disco","value":0.8817196273397615,"text":"58 GB libres","level":"warm"}],"orchestrator":{"text":"inactivo","tone":"neutral"}}"#
        let status = try ContractCodec.decode(ViewStatus.self, from: Data(json.utf8))
        XCTAssertEqual(status.gauges.map(\.id), ["claude_week", "claude_today", "claude_five_hours", "memory", "cpu", "disk"])
        XCTAssertEqual(status.gauges.map(\.text), ["83%", "0/6%", "0%", "39%", "24%", "58 GB libres"])
    }

    func testDocumentReviewOpenedFromTheCardPathOffersApproveAndRequestChanges() throws {
        let approve = #"{"id":"accept:revision-1@1","label":"Approve","style":"primary","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":true,"stamp":"Approved"}"#
        let changes = #"{"id":"changes:revision-1@1","label":"Request changes","style":"secondary","presentation":"button","input":{"kind":"text","required":true,"multiline":true},"attachments":true,"enabled":true}"#
        let json = #"{"protocol":1,"view":"card","key":"card:revision-1","client":"ios","version":"v0123456789abcdef","cursor":"c1.abc","data_epoch":"epoch","status":{"version":"s1","summary":"1 question","badge":1,"counts":[],"gauges":[],"orchestrator":{"text":"Ready","tone":"success"}},"sections":[{"id":"card","collapsed":false,"items":[{"type":"card","id":"revision-1","icon":"document","title":"Plan","detail":"included","actions":[\#(approve),\#(changes)]},{"type":"actions","actions":[\#(approve)]}]}],"notices":[]}"#
        let document = try ContractCodec.decode(ViewDocument.self, from: Data(json.utf8))
        XCTAssertEqual(document.reviewActions(forKey: "card:revision-1").map(\.label), ["Approve", "Request changes"])
        XCTAssertEqual(document.reviewActions(forKey: "card:revision-1").map(\.id), ["accept:revision-1@1", "changes:revision-1@1"])
        XCTAssertNil(document.documentCard(forKey: "document:revision-1"))
        XCTAssertEqual(document.reviewActions(forKey: "document:revision-1").map(\.label), ["Approve"])
    }

    func testDecodesAndReencodesViewWithInternallyTaggedItems() throws {
        let json = #"{"protocol":1,"view":"home","key":"home","client":"ios","version":"v0123456789abcdef","cursor":"c1.abc","data_epoch":"epoch","status":{"version":"s1","summary":"1 question","badge":1,"counts":[],"gauges":[],"orchestrator":{"text":"Ready","tone":"success"}},"sections":[{"id":"queue","collapsed":false,"card_ids":["i-42","i-43"],"items":[{"type":"card","id":"i-42","icon":"question","title":"Review this","badges":[],"context":[],"timestamps":[],"detail":"included","attachments":[],"actions":[]},{"type":"markdown","text":"Hello","truncated":false,"commentable":true}]}],"notices":[]}"#.data(using: .utf8)!
        let document = try ContractCodec.decode(ViewDocument.self, from: json)
        XCTAssertEqual(document.protocolVersion, 1)
        XCTAssertEqual(document.sections[0].items.count, 2)
        XCTAssertEqual(document.sections[0].cardIds, ["i-42", "i-43"])
        guard case let .card(card) = document.sections[0].items[0] else { return XCTFail("expected card") }
        XCTAssertEqual(card.id, "i-42")
        let encoded = try ContractCodec.encode(document)
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: encoded) as? [String: Any])
        let sections = try XCTUnwrap(object["sections"] as? [[String: Any]])
        let items = try XCTUnwrap(sections[0]["items"] as? [[String: Any]])
        XCTAssertEqual(sections[0]["card_ids"] as? [String], ["i-42", "i-43"])
        XCTAssertEqual(items[0]["type"] as? String, "card")
        XCTAssertNil(items[0]["kind"])
    }

    func testTaggedEnumsAndActionRoundTrip() throws {
        let linkJSON = #"{"kind":"local","local":{"kind":"review_clone","repo":"alondra","branch":"feature/views","pr":214}}"#.data(using: .utf8)!
        let link = try JSONDecoder().decode(Link.self, from: linkJSON)
        XCTAssertEqual(try JSONDecoder().decode(Link.self, from: JSONEncoder().encode(link)), link)

        let inputJSON = #"{"kind":"text","placeholder":"Reply","required":true,"multiline":true}"#.data(using: .utf8)!
        let input = try JSONDecoder().decode(Input.self, from: inputJSON)
        XCTAssertEqual(try JSONDecoder().decode(Input.self, from: JSONEncoder().encode(input)), input)

        let actionJSON = #"{"protocol":1,"command_id":"5f0c2d9e-8d0b-4c71-9b0e-2b6f5d8f1a10","data_epoch":"epoch","client":"ios","lang":"es","action_id":"ans:i-42@117","option_id":"other","input":{"text":"Keep tokens","attachments":["att:1"],"comments":[{"anchor":"block-1","text":"Clarify","quote":"First block"}],"fields":{"enabled":true}}}"#.data(using: .utf8)!
        let request = try JSONDecoder().decode(ActionRequest.self, from: actionJSON)
        XCTAssertEqual(request.actionId, "ans:i-42@117")
        XCTAssertEqual(request.input.comments?.first?.quote, "First block")
        XCTAssertEqual(try JSONDecoder().decode(ActionRequest.self, from: JSONEncoder().encode(request)), request)
    }

    func testChangeFeedAndResponseEnumsMatchWireNames() throws {
        let feedJSON = #"{"protocol":1,"cursor":"c1.abc","data_epoch":"epoch","status":{"version":"s1","summary":"Ready","badge":0,"counts":[],"gauges":[],"orchestrator":{"text":"Ready","tone":"success"}},"stale":["task:t-12"],"reset":false,"superseded":true}"#.data(using: .utf8)!
        let feed = try JSONDecoder().decode(ViewFeed.self, from: feedJSON)
        XCTAssertEqual(feed.stale, ["task:t-12"])
        XCTAssertTrue(feed.superseded)
        let responseJSON = #"{"protocol":1,"outcome":"no_longer_available","message":"No longer pending"}"#.data(using: .utf8)!
        let response = try JSONDecoder().decode(ActionResponse.self, from: responseJSON)
        XCTAssertEqual(response.outcome, .noLongerAvailable)
    }

    func testRowIgnoresRemovedPresentationFieldsAndAttachmentDigestRoundTrips() throws {
        let rowJSON = #"{"id":"worker-1","link":{"kind":"view","key":"node:worker-1"},"icon":"node","title":"Worker","subtitle":"Working","badges":[],"trailing":{"kind":"count","value":1},"depth":0,"struck":false,"state_key":"busy","tooltip":"Worker · Working\nRecent summary","children":[],"actions":[]}"#.data(using: .utf8)!
        let row = try ContractCodec.decode(Row.self, from: rowJSON)
        let encodedRow = try XCTUnwrap(JSONSerialization.jsonObject(with: ContractCodec.encode(row)) as? [String: Any])
        XCTAssertNil(encodedRow["state_key"])
        XCTAssertNil(encodedRow["tooltip"])

        let attachmentJSON = #"{"id":"att-1","name":"notes.txt","kind":"text","size":12,"fetch":"private/ref","digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#.data(using: .utf8)!
        let attachment = try ContractCodec.decode(Attachment.self, from: attachmentJSON)
        XCTAssertEqual(attachment.digest, String(repeating: "a", count: 64))
        XCTAssertEqual(try ContractCodec.decode(Attachment.self, from: ContractCodec.encode(attachment)), attachment)
    }

    func testConditionDecodesScalarAndAnyOfVisibilityRules() throws {
        let scalar = try ContractCodec.decode(Condition.self, from: Data(#"{"field":"frequency","equals":"daily"}"#.utf8))
        XCTAssertEqual(scalar.equals, .string("daily"))
        XCTAssertNil(scalar.anyOf)
        XCTAssertTrue(scalar.matches(.string("daily")))
        XCTAssertFalse(scalar.matches(.string("weekly")))

        let anyOf = try ContractCodec.decode(Condition.self, from: Data(#"{"field":"frequency","any_of":["every_hours","every_days","every_months"]}"#.utf8))
        XCTAssertNil(anyOf.equals)
        XCTAssertEqual(anyOf.anyOf, [.string("every_hours"), .string("every_days"), .string("every_months")])
        XCTAssertTrue(anyOf.matches(.string("every_months")))
        XCTAssertFalse(anyOf.matches(.string("weekly")))
        let encoded = try XCTUnwrap(JSONSerialization.jsonObject(with: ContractCodec.encode(anyOf)) as? [String: Any])
        XCTAssertEqual(encoded["any_of"] as? [String], ["every_hours", "every_days", "every_months"])
    }

    /// Whether an action takes its card out is the server's word, read from `resolves_card`.
    /// A daemon older than the field says nothing; then only a message keeps its card.
    func testAnActionCarriesWhetherItTakesItsCardOutAndTheRequestKeepsItOnTheDevice() throws {
        func action(_ extra: String) throws -> Action {
            try ContractCodec.decode(Action.self, from: Data("""
            {"id":"reply:q-1@3","label":"Reply","style":"secondary","presentation":"button","input":{"kind":"none"},
             "attachments":false,"enabled":true\(extra)}
            """.utf8))
        }
        XCTAssertEqual(try action(#","resolves_card":true"#).resolvesCard, true)
        XCTAssertEqual(try action(#","resolves_card":false"#).resolvesCard, false)
        XCTAssertNil(try action("").resolvesCard)

        let request = { (resolves: Bool?) in
            ActionRequest(commandId: "c", dataEpoch: "e", client: .ios, lang: "es", actionId: "reply:q-1@3", resolvesCard: resolves)
        }
        XCTAssertTrue(request(true).resolvesItsCard, "a reply the server says answers its question")
        XCTAssertFalse(request(false).resolvesItsCard)
        XCTAssertFalse(request(nil).resolvesItsCard, "an older daemon only ever sent a reply to the node")
        XCTAssertTrue(ActionRequest(commandId: "c", dataEpoch: "e", client: .ios, lang: "es", actionId: "ans:q-1@3").resolvesItsCard)
        let sent = try XCTUnwrap(JSONSerialization.jsonObject(with: ContractCodec.encode(request(true))) as? [String: Any])
        XCTAssertNil(sent["resolves_card"], "the server decides it again; the device never sends it")
        XCTAssertEqual(Set(sent.keys), ["protocol", "command_id", "data_epoch", "client", "lang", "action_id", "input"])
    }
}
