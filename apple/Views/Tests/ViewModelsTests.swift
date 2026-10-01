import Foundation
import XCTest
import AlondraViews

final class ViewModelsTests: XCTestCase {
    func testDecodesRustGoldenViews() throws {
        let env = ProcessInfo.processInfo.environment
        let root = URL(fileURLWithPath: try XCTUnwrap(env["TEST_SRCDIR"]))
            .appendingPathComponent(env["TEST_WORKSPACE"] ?? "_main")
            .appendingPathComponent("fixtures/views")
        for name in ["home-ios", "home-ios-es", "home-tui", "card-question", "card-pr", "card-delivery"] {
            let data = try Data(contentsOf: root.appendingPathComponent("\(name).json"))
            let view = try ViewsCodec.decode(ViewDocument.self, from: data)
            XCTAssertEqual(view.protocolVersion, 1, name)
            if name.hasPrefix("home-ios") {
                XCTAssertEqual(view.sections.first?.cardIds?.count, 4, name)
                XCTAssertEqual(view.sections.first?.items.count, 0, name)
            }
        }
    }

    func testDecodesAndReencodesViewWithInternallyTaggedItems() throws {
        let json = #"{"protocol":1,"view":"home","key":"home","client":"ios","version":"v0123456789abcdef","cursor":"c1.abc","data_epoch":"epoch","status":{"version":"s1","summary":"1 question","badge":1,"counts":[],"gauges":[],"orchestrator":{"text":"Ready","tone":"success"}},"sections":[{"id":"queue","collapsed":false,"card_ids":["i-42","i-43"],"items":[{"type":"card","id":"i-42","icon":"question","title":"Review this","badges":[],"context":[],"timestamps":[],"detail":"included","attachments":[],"quotes":[],"children":[],"actions":[]},{"type":"markdown","text":"Hello","truncated":false,"commentable":true}]}],"notices":[]}"#.data(using: .utf8)!
        let document = try ViewsCodec.decode(ViewDocument.self, from: json)
        XCTAssertEqual(document.protocolVersion, 1)
        XCTAssertEqual(document.sections[0].items.count, 2)
        XCTAssertEqual(document.sections[0].cardIds, ["i-42", "i-43"])
        guard case let .card(card) = document.sections[0].items[0] else { return XCTFail("expected card") }
        XCTAssertEqual(card.id, "i-42")
        let encoded = try ViewsCodec.encode(document)
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

    func testSidebarStateAndAttachmentDigestDecodeAndRoundTrip() throws {
        let rowJSON = #"{"id":"worker-1","link":{"kind":"view","key":"node:worker-1"},"icon":"node","title":"Worker","subtitle":"Working","badges":[],"trailing":{"kind":"count","value":1},"depth":0,"struck":false,"state_key":"busy","tooltip":"Worker · Working\nRecent summary","children":[],"actions":[]}"#.data(using: .utf8)!
        let row = try ViewsCodec.decode(Row.self, from: rowJSON)
        XCTAssertEqual(row.stateKey, "busy")
        XCTAssertEqual(row.tooltip, "Worker · Working\nRecent summary")
        XCTAssertEqual(try ViewsCodec.decode(Row.self, from: ViewsCodec.encode(row)), row)

        let attachmentJSON = #"{"id":"att-1","name":"notes.txt","kind":"text","size":12,"fetch":"private/ref","digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#.data(using: .utf8)!
        let attachment = try ViewsCodec.decode(Attachment.self, from: attachmentJSON)
        XCTAssertEqual(attachment.digest, String(repeating: "a", count: 64))
        XCTAssertEqual(try ViewsCodec.decode(Attachment.self, from: ViewsCodec.encode(attachment)), attachment)
    }
}
