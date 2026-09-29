import AlondraContracts
import Foundation
import XCTest

/// Every canonical v2 fixture must decode into the Swift DTOs and re-encode without losing
/// a field, so the Apple contract cannot drift silently from the Rust one.
final class DomainV2FixtureTests: XCTestCase {
    private func fixture(_ path: String) throws -> Data {
        let env = ProcessInfo.processInfo.environment
        let root: URL
        if let runfiles = env["TEST_SRCDIR"] {
            root = URL(fileURLWithPath: runfiles).appendingPathComponent(env["TEST_WORKSPACE"] ?? "_main")
        } else {
            root = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
                .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        }
        return try Data(contentsOf: root.appendingPathComponent(path))
    }

    /// Rust omits nothing but null optionals; `attachments` is `#[serde(default)]`, so an
    /// empty list and an absent key are the same wire value.
    private func withoutNulls(_ value: Any) -> Any {
        if let object = value as? [String: Any] {
            return object.filter { !($0.value is NSNull) && !($0.key == "attachments" && ($0.value as? [Any])?.isEmpty == true) }
                .mapValues(withoutNulls)
        }
        if let array = value as? [Any] { return array.map(withoutNulls) }
        return value
    }

    private func assertRoundTrip<Value: Codable>(_ type: Value.Type, _ json: Any, _ label: String,
                                                 file: StaticString = #filePath, line: UInt = #line) throws {
        let bytes = try JSONSerialization.data(withJSONObject: json)
        let decoded = try DomainV2.decode(type, from: bytes)
        let encoded = try JSONSerialization.jsonObject(with: DomainV2.encode(decoded))
        XCTAssertEqual(withoutNulls(encoded) as? NSObject, withoutNulls(json) as? NSObject,
                       "\(label) lost or changed a field", file: file, line: line)
    }

    func testEveryProjectionFixtureDecodesWithoutLosingAField() throws {
        let object = try XCTUnwrap(JSONSerialization.jsonObject(
            with: fixture("apple/APIClient/Tests/domain-v2-wire-fixtures.json")) as? [String: Any])
        XCTAssertEqual(Set(object.keys), ["information", "question", "accepted", "completed", "pr_question"])
        for (name, projection) in object {
            try assertRoundTrip(DomainV2.DomainProjection.self, projection, name)
        }
    }

    func testTextCompletionEnvelopesDecodeExactly() throws {
        let trace = try XCTUnwrap(JSONSerialization.jsonObject(
            with: fixture("fixtures/domain-v2/text-completion.commands.json")) as? [String: Any])
        let steps = try XCTUnwrap(trace["steps"] as? [[String: Any]])
        XCTAssertFalse(steps.isEmpty)
        for step in steps {
            let envelope = try XCTUnwrap(step["envelope"])
            try assertRoundTrip(DomainV2.CommandEnvelope.self, envelope, "\(step["actor"] ?? "")")
        }
    }

    func testEveryAcceptanceCommandStepDecodesExactly() throws {
        let catalog = try XCTUnwrap(JSONSerialization.jsonObject(
            with: fixture("fixtures/domain-v2/acceptance.json")) as? [String: Any])
        var kinds = Set<String>()
        func visit(_ value: Any) throws {
            if let object = value as? [String: Any] {
                if object["type"] as? String == "command", let command = object["command"] as? [String: Any] {
                    kinds.insert(try XCTUnwrap(command["kind"] as? String))
                    try assertRoundTrip(DomainV2.Command.self, command, command["kind"] as? String ?? "")
                }
                for child in object.values { try visit(child) }
            } else if let array = value as? [Any] {
                for child in array { try visit(child) }
            }
        }
        try visit(catalog)
        XCTAssertTrue(kinds.isSuperset(of: ["mark_read", "answer_question", "present_result", "accept_delivery",
                                            "submit_feedback", "authorize_merge", "complete_task"]), "\(kinds)")
    }

    func testCommandOutcomeCarriesEveryEffectKind() throws {
        let json: [String: Any] = [
            "revision": 9, "changed": true, "created_ids": ["q"],
            "effects": [
                ["kind": "notify_user", "task_id": "t", "interaction_id": "q"],
                ["kind": "notify_agent", "agent_id": "a", "task_id": "t", "interaction_id": "q", "message": "Answer"],
                ["kind": "notify_orchestrator", "interaction_id": "o"],
            ],
        ]
        try assertRoundTrip(DomainV2.CommandOutcome.self, json, "outcome")
    }

    func testEveryTypedErrorCodeDecodes() throws {
        for code in DomainV2.ErrorCode.allCases {
            let json: [String: Any] = ["code": code.rawValue, "message": "Synthetic", "current_revision": 3, "subject": "q"]
            let error = try DomainV2.decode(DomainV2.DomainError.self, from: JSONSerialization.data(withJSONObject: json))
            XCTAssertEqual(error.errorCode, code)
            XCTAssertEqual(error.currentRevision, 3)
        }
        let unknown = try DomainV2.decode(DomainV2.DomainError.self,
            from: Data(#"{"code":"future_rule","message":"Later","current_revision":null,"subject":null}"#.utf8))
        XCTAssertNil(unknown.errorCode)
        XCTAssertEqual(unknown.code, "future_rule")
    }

    func testProjectionActionKindsAreAllKnown() throws {
        let fixtures = try JSONDecoder().decode([String: DomainV2.DomainProjection].self,
            from: fixture("apple/APIClient/Tests/domain-v2-wire-fixtures.json"))
        func actions(_ items: [DomainV2.InboxItem]) -> [DomainV2.AvailableAction] {
            items.flatMap { $0.availableActions + actions($0.children) }
        }
        let all = fixtures.values.flatMap { actions($0.inbox) }
        XCTAssertFalse(all.isEmpty)
        XCTAssertTrue(all.allSatisfy { $0.actionKind != nil }, all.map(\.kind).description)
    }
}
