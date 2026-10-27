import AlondraContract
import AlondraFeatures
import ComposableArchitecture
import Foundation
import XCTest

@MainActor
final class RequestOptionsFeatureTests: XCTestCase {
    private func fields() throws -> [Field] {
        let json = #"""
        [
          {"id":"model","label":"Model","kind":{"kind":"choice","items":[{"id":"auto","label":"Automatic"},{"id":"codex/precise","label":"Precise","group":"Codex"},{"id":"claude/large","label":"Large","group":"Claude"}]},"value":"auto","required":false,"reload_on_change":false},
          {"id":"account","label":"Account","kind":{"kind":"choice","items":[{"id":"auto","label":"Automatic"},{"id":"codex/work","label":"Work","available_when":{"field":"model","any_of":["auto","codex/precise"]}}]},"value":"auto","required":false,"reload_on_change":false},
          {"id":"reasoning_effort","label":"Reasoning effort","kind":{"kind":"choice","items":[{"id":"auto","label":"Native default"},{"id":"high","label":"High","available_when":{"field":"model","equals":"codex/precise"}}]},"value":"auto","required":false,"reload_on_change":false},
          {"id":"allow_model_substitution","label":"Allow model substitution","kind":{"kind":"toggle"},"value":true,"required":false,"reload_on_change":false}
        ]
        """#
        return try JSONDecoder().decode([Field].self, from: Data(json.utf8))
    }

    func testExplicitModelDefaultsToNoSubstitutionAndChangingModelResetsNativeEffort() async throws {
        let availableFields = try fields()
        let store = TestStore(initialState: RequestOptionsFeature.State(fields: availableFields, values: ["model": .string("auto"), "reasoning_effort": .string("auto"), "allow_model_substitution": .bool(true)])) { RequestOptionsFeature() }
        await store.send(.choose("model", "codex/precise")) {
            $0.values["model"] = .string("codex/precise")
            $0.values["allow_model_substitution"] = .bool(false)
            $0.selectionLabels["model"] = "Precise"
        }
        for _ in 0..<3 { await store.receive(\.delegate) }
        await store.send(.choose("reasoning_effort", "high")) {
            $0.values["reasoning_effort"] = .string("high")
            $0.selectionLabels["reasoning_effort"] = "High"
        }
        await store.receive(\.delegate)
        await store.send(.choose("model", "claude/large")) {
            $0.values["model"] = .string("claude/large")
            $0.values["reasoning_effort"] = .string("auto")
            $0.selectionLabels["model"] = "Large"
        }
        for _ in 0..<4 { await store.receive(\.delegate) }
        XCTAssertFalse(store.state.isAvailable(try XCTUnwrap(store.state.choices("account").first { $0.id == "codex/work" })))
    }

    func testExplicitSubstitutionChoiceSurvivesAutomaticAndExplicitModelChanges() async throws {
        let availableFields = try fields()
        let store = TestStore(initialState: RequestOptionsFeature.State(fields: availableFields, values: ["model": .string("codex/precise"), "allow_model_substitution": .bool(false)])) { RequestOptionsFeature() }
        await store.send(.toggle("allow_model_substitution", true)) {
            $0.values["allow_model_substitution"] = .bool(true)
            $0.values["substitution_was_edited"] = .bool(true)
        }
        await store.receive(\.delegate)
        await store.receive(\.delegate)
        await store.send(.choose("model", "auto")) {
            $0.values["model"] = .string("auto")
            $0.selectionLabels["model"] = "Automatic"
        }
        await store.receive(\.delegate)
        await store.send(.choose("model", "claude/large")) {
            $0.values["model"] = .string("claude/large")
            $0.selectionLabels["model"] = "Large"
        }
        await store.receive(\.delegate)
        XCTAssertEqual(store.state.values["allow_model_substitution"], .bool(true))
    }

    func testChangingOnlyTheModelDoesNotPinAnAccountButChoosingAnAccountDoes() async throws {
        var options = try fields()
        let toggle = #"{"id":"pin_account","label":"Keep this account","kind":{"kind":"toggle"},"value":false,"required":false,"reload_on_change":false}"#
        options.append(try JSONDecoder().decode(Field.self, from: Data(toggle.utf8)))
        let store = TestStore(initialState: RequestOptionsFeature.State(fields: options, values: ["model": .string("auto"), "account": .string("auto"), "pin_account": .bool(false), "allow_model_substitution": .bool(true)])) { RequestOptionsFeature() }
        await store.send(.choose("model", "codex/precise")) {
            $0.values["model"] = .string("codex/precise")
            $0.values["allow_model_substitution"] = .bool(false)
            $0.selectionLabels["model"] = "Precise"
        }
        for _ in 0..<3 { await store.receive(\.delegate) }
        XCTAssertEqual(store.state.values["pin_account"], .bool(false))
        await store.send(.choose("account", "codex/work")) {
            $0.values["account"] = .string("codex/work")
            $0.values["pin_account"] = .bool(true)
            $0.selectionLabels["account"] = "Work"
        }
        await store.receive(\.delegate)
        await store.receive(\.delegate)
        await store.send(.toggle("pin_account", false)) { $0.values["pin_account"] = .bool(false) }
        await store.receive(\.delegate)
    }

    func testCatalogRefreshRetainsTheChosenNameAndIntentAndUnavailableChoicesCannotBePicked() async throws {
        var state = RequestOptionsFeature.State(fields: try fields(), values: ["model": .string("codex/precise"), "account": .string("codex/work"), "allow_model_substitution": .bool(true), "substitution_was_edited": .bool(true)])
        let refreshed = try fields().filter { $0.id != "model" }
        state.refresh(fields: refreshed)
        XCTAssertEqual(state.selectedLabel("model"), "Codex · Precise")
        XCTAssertEqual(state.values["account"], .string("codex/work"))
        XCTAssertEqual(state.values["substitution_was_edited"], .bool(true))
        XCTAssertEqual(state.choicesIncludingSelection("model").last?.disabledReason, String(localized: "Unavailable in the current catalog"))
        let availableFields = try fields()
        let store = TestStore(initialState: RequestOptionsFeature.State(fields: availableFields, values: ["model": .string("claude/large")])) { RequestOptionsFeature() }
        await store.send(.choose("reasoning_effort", "high"))
        await store.send(.choose("account", "codex/work"))
    }
}
