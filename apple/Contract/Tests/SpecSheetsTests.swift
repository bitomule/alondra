import AlondraContract
import Foundation
import XCTest

final class SpecSheetsTests: XCTestCase {
    private let sheet = #"""
    {"type":"spec_sheet","id":"s1","spec":"billing.partial","title":"Partial refunds","mark":{"change":"new","label":"new"},
     "explanation":"A customer gets back any amount.",
     "decisions":{"label":"Relies on:","items":[{"title":"A refund never exceeds what is left","link":{"kind":"view","key":"task_specs:t-1:decision:d1"}}]},
     "parts":[{"id":"flow.mmd","kind":"diagram","name":"flow.mmd","title":"Diagram “Flow”","alt":"Diagram Flow","code":"flowchart LR\n  A --> B"},
              {"id":"sheet.png","kind":"mockup","name":"sheet.png","title":"Mockup","alt":"Mockup of the spec","picture":{"url":"/files?path=files/images/x.png","width":1170,"height":2532,"bytes":10}}],
     "scenarios":[{"id":"billing.partial/part","title":"Part of an order","kind":"test","collapsed":false,
        "lanes":[{"kind":"given","label":"Given","steps":[{"text":"a paid order"}]},
                 {"kind":"when","label":"When","steps":[{"text":"refunded","table":[["a","b"],["1","2"]],"doc":"text"}]},
                 {"kind":"then","label":"Then","steps":[{"text":"received"}]}],
        "examples":{"label":"Examples","rows":[["x"],["1"]]},
        "result":{"state":{"icon":"check","tone":"success","text":"Validated"},
                  "verdicts":[{"line":{"icon":"warning","tone":"warning","text":"Judgment"},"reasoning":"why","nearest_label":"Closest","nearest_wrong":"wrong","evidence":[]}]},
        "anchor":{"id":"a","quote":"q"},"edit":{"kind":"view","key":"task_specs:t-1:scenario:c1"}}],
     "unchanged":{"label":"12 unchanged scenarios","scenarios":[]},
     "anchor":{"id":"billing.partial","quote":"billing.partial"},"add":[{"title":"Add a scenario","link":{"kind":"view","key":"k"}}]}
    """#

    func testASpecSheetDecodesWithItsPartsLanesAndResult() throws {
        let item = try JSONDecoder().decode(ViewItem.self, from: Data(sheet.utf8))
        guard case let .specSheet(spec) = item else { return XCTFail("\(item)") }
        XCTAssertEqual(spec.title, "Partial refunds")
        XCTAssertEqual(spec.mark?.change, .new)
        XCTAssertEqual(spec.parts.map(\.kind), [.diagram, .mockup])
        XCTAssertEqual(spec.parts[1].picture?.aspectRatio ?? 0, 1170.0 / 2532.0, accuracy: 0.0001)
        XCTAssertEqual(spec.scenarios[0].lanes.map(\.kind), [.given, .when, .then])
        XCTAssertEqual(spec.scenarios[0].lanes[1].steps[0].table, [["a", "b"], ["1", "2"]])
        XCTAssertEqual(spec.scenarios[0].result?.verdicts[0].nearestWrong, "wrong")
        XCTAssertEqual(spec.unchanged?.label, "12 unchanged scenarios")
        XCTAssertEqual(spec.anchors.map(\.id), ["billing.partial", "a"])
    }

    func testASpecSheetSurvivesAReencode() throws {
        let item = try JSONDecoder().decode(ViewItem.self, from: Data(sheet.utf8))
        let again = try JSONDecoder().decode(ViewItem.self, from: try ContractCodec.encode(item))
        XCTAssertEqual(again, item)
    }

    func testAnUnknownChangeOrLaneOrPartKindDrawsPlainInsteadOfFailingTheView() throws {
        let newer = sheet
            .replacingOccurrences(of: #""change":"new""#, with: #""change":"merged""#)
            .replacingOccurrences(of: #""kind":"given""#, with: #""kind":"background""#)
            .replacingOccurrences(of: #""kind":"mockup""#, with: #""kind":"video""#)
        guard case let .specSheet(spec) = try JSONDecoder().decode(ViewItem.self, from: Data(newer.utf8)) else { return XCTFail() }
        XCTAssertEqual(spec.mark?.change, .unchanged)
        XCTAssertEqual(spec.scenarios[0].lanes[0].kind, .other)
        XCTAssertEqual(spec.parts[1].kind, .other)
    }

    func testADecisionSheetDecodes() throws {
        let json = #"{"type":"decision_sheet","id":"d1","decision":"refund-limit","title":"A refund never exceeds what is left","status":"accepted","sections":[{"label":"Context","text":"why"}],"superseded_by":{"label":"Replaced by","items":[{"title":"Other"}]}}"#
        guard case let .decisionSheet(decision) = try JSONDecoder().decode(ViewItem.self, from: Data(json.utf8)) else { return XCTFail() }
        XCTAssertEqual(decision.sections.map(\.label), ["Context"])
        XCTAssertEqual(decision.supersededBy?.items.map(\.title), ["Other"])
    }

    func testTheStepsAndMermaidFieldsDecodeAndEncode() throws {
        for kind in ["steps", "mermaid"] {
            let json = #"{"kind":"\#(kind)","placeholder":"Given …"}"#
            let field = try JSONDecoder().decode(FieldKind.self, from: Data(json.utf8))
            XCTAssertEqual(try JSONDecoder().decode(FieldKind.self, from: try JSONEncoder().encode(field)), field)
        }
    }
}
