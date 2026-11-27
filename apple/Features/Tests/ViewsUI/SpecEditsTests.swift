import AlondraContract
import AlondraDesign
import AlondraFeatures
import XCTest

final class SpecEditsTests: XCTestCase {
    private func request(_ extra: String, fields: [String: JSONValue] = [:], command: String = "c1") -> ActionRequest {
        ActionRequest(commandId: command, dataEpoch: "e", client: .ios, lang: "en", actionId: "spec_edit:t-1@7~\(extra)", optionId: nil,
                      input: ActionInput(fields: fields), resolvesCard: nil)
    }

    private func scenario(_ id: String, key: String) -> SheetScenario {
        SheetScenario(id: id, title: "Part of an order", kind: "test",
                      lanes: [SheetLane(kind: .given, label: "Partiendo de", steps: [SheetStep(text: "an order")])], edit: .view(key: key))
    }

    private func sheet() -> SpecSheet {
        SpecSheet(id: "s", spec: "billing.partial", title: "Partial refunds", explanation: "old", scenarios: [scenario("a", key: "task_specs:t-1:scenario:c1")],
                  unchanged: SheetUnchanged(label: "1 unchanged scenario", scenarios: [scenario("b", key: "task_specs:t-1:scenario:c2")]),
                  edit: .view(key: "task_specs:t-1:spec:s1"))
    }

    func testAnEditNamesTheScreenThatEditsItsTarget() {
        let cases: [(String, SpecEdits.Operation)] = [
            ("sc-c1", .scenario("task_specs:t-1:scenario:c1")), ("sc-new-s1", .newScenario("task_specs:t-1:spec:s1")),
            ("rmsc-c1", .removeScenario("task_specs:t-1:scenario:c1")), ("spec-s1", .spec("task_specs:t-1:spec:s1")),
            ("dec-d1", .decision("task_specs:t-1:decision:d1")), ("part-diagram-sa-b2", .diagram("task_specs:t-1:part:diagram:sa:b2")),
            ("part-mockup-sa-b2", .other("task_specs:t-1:part:mockup:sa:b2")), ("rmpart-mockup-sa-b2", .removePart("task_specs:t-1:part:mockup:sa:b2")),
            ("level", .other("task_specs:t-1")),
        ]
        for (extra, expected) in cases {
            var edits = SpecEdits()
            edits.begin(request(extra), form: nil)
            XCTAssertEqual(edits.edits.first?.operation, expected, extra)
        }
    }

    func testAnEditedScenarioShowsAtOnceInItsLanesAndIsMarkedAsSaving() throws {
        var edits = SpecEdits()
        edits.begin(request("sc-c1", fields: ["title": .string("Part of the order"), "steps": .string("Given a paid order\nWhen 10 EUR are refunded\nThen 20 EUR are left")]), form: nil)
        let shown = try XCTUnwrap(edits.patched(sheet()))
        XCTAssertEqual(shown.scenarios[0].title, "Part of the order")
        XCTAssertEqual(shown.scenarios[0].lanes.map(\.kind), [.given, .when, .then])
        XCTAssertEqual(shown.scenarios[0].lanes[0].label, "Partiendo de", "the lane keeps the daemon's words")
        XCTAssertEqual(shown.unchanged?.scenarios[0].title, "Part of an order", "another scenario is left alone")
        XCTAssertEqual(edits.saving, ["task_specs:t-1:scenario:c1": .saving])
    }

    func testARemovedScenarioAndASpecDisappearAndAnAddedScenarioAppears() throws {
        var edits = SpecEdits()
        edits.begin(request("rmsc-c2", command: "c2"), form: nil)
        XCTAssertEqual(try XCTUnwrap(edits.patched(sheet())).unchanged?.scenarios.count, 0)
        edits.begin(request("sc-new-s1", fields: ["id": .string("new"), "title": .string("A new one"), "steps": .string("Given a\nThen b")], command: "c3"), form: nil)
        XCTAssertEqual(try XCTUnwrap(edits.patched(sheet())).scenarios.map(\.title), ["Part of an order", "A new one"])
        edits.begin(request("rmspec-s1", command: "c4"), form: nil)
        XCTAssertNil(edits.patched(sheet()))
    }

    func testAnEditTheMacRefusesGoesBackAndComesBackWithItsFields() throws {
        var edits = SpecEdits()
        let sent = request("sc-c1", fields: ["title": .string("Changed")])
        edits.begin(sent, form: nil)
        edits.settle(sent, applied: false, shown: nil)
        XCTAssertEqual(try XCTUnwrap(edits.patched(sheet())).scenarios[0].title, "Part of an order", "it is back to what it was")
        XCTAssertTrue(edits.saving.isEmpty)
        let again = try XCTUnwrap(edits.refused(sent))
        XCTAssertEqual(again.fields["title"], .string("Changed"))
        XCTAssertTrue(edits.isEmpty)
    }

    func testAnAppliedEditStaysUntilANewerViewArrives() throws {
        var edits = SpecEdits()
        let sent = request("spec-s1", fields: ["title": .string("Refunds")])
        edits.begin(sent, form: nil)
        edits.settle(sent, applied: true, shown: "v1")
        edits.reconcile("v1")
        XCTAssertEqual(try XCTUnwrap(edits.patched(sheet())).title, "Refunds")
        edits.reconcile("v2")
        XCTAssertTrue(edits.isEmpty)
    }

    func testAnEditTheMacHasNotConfirmedInTimeSaysSoAndKeepsShowing() throws {
        var edits = SpecEdits()
        let sent = request("spec-s1", fields: ["title": .string("Refunds")])
        edits.begin(sent, form: nil)
        XCTAssertTrue(edits.expire(sent.deliveryCorrelationID))
        XCTAssertEqual(edits.saving, ["task_specs:t-1:spec:s1": .overdue])
        XCTAssertEqual(try XCTUnwrap(edits.patched(sheet())).title, "Refunds")
        XCTAssertFalse(edits.expire(sent.deliveryCorrelationID), "it says so once")
    }

    func testOnlyTheFormsOfASpecsScreenOpenOverIt() {
        XCTAssertTrue(SpecEdits.isEditorForm("task_specs:t-1:scenario:c1"))
        XCTAssertFalse(SpecEdits.isEditorForm("task_specs:t-1"))
        XCTAssertFalse(SpecEdits.isEditorForm("card:i-1"))
    }
}
