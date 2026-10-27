import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import Foundation
import XCTest

@MainActor
final class ModelFormIntentTests: XCTestCase {
    func testProfileAndActiveModelRoutesUseTheExistingInlineFormScreen() {
        for key in ["model_profile:codex-orchestrator", "model_profile:claude-general", "task_selection:t-1"] {
            XCTAssertTrue(ViewsScreenFeature.State(key: .init(rawValue: key), owner: "settings").hasInlineForm, key)
        }
        XCTAssertFalse(ViewsScreenFeature.State(key: .init(rawValue: "accounts"), owner: "settings").hasInlineForm)
    }

    private func document(version: String, epoch: String? = nil) throws -> ViewDocument {
        var object = try XCTUnwrap(try JSONSerialization.jsonObject(with: JSONEncoder().encode(Fixtures.document("new-request-ios"))) as? [String: Any])
        object["key"] = "task_selection:t-1"; object["view"] = "task_selection"; object["version"] = version
        if let epoch { object["data_epoch"] = epoch }
        let fields = #"""
        [
          {"id":"model","label":"Model","kind":{"kind":"choice","items":[{"id":"auto","label":"Automatic"},{"id":"claude/exact","label":"Exact","group":"Claude"},{"id":"codex/precise","label":"Precise","group":"Codex"}]},"value":"claude/exact","required":false,"reload_on_change":false},
          {"id":"account","label":"Account","kind":{"kind":"choice","items":[{"id":"auto","label":"Automatic"},{"id":"codex/work","label":"Work","group":"Codex"}]},"value":"auto","required":false,"reload_on_change":false},
          {"id":"reasoning_effort","label":"Reasoning effort","kind":{"kind":"choice","items":[{"id":"auto","label":"Native default"},{"id":"high","label":"High"}]},"value":"high","required":false,"reload_on_change":false},
          {"id":"allow_model_substitution","label":"Allow model substitution","kind":{"kind":"toggle"},"value":true,"required":false,"reload_on_change":false},
          {"id":"pin_account","label":"Keep this account","kind":{"kind":"toggle"},"value":false,"required":false,"reload_on_change":false}
        ]
        """#
        let action: [String: Any] = ["id": "task_selection:t-1@0~" + version, "label": "Change selection", "style": "primary", "presentation": "button", "input": ["kind": "none"], "attachments": false, "enabled": true]
        let form: [String: Any] = ["type": "form", "id": "task_selection", "substitution_was_edited": true, "fields": try JSONSerialization.jsonObject(with: Data(fields.utf8)), "submit": action]
        object["sections"] = [["id":"form","collapsed":false,"items":[form]]]
        return try ContractCodec.decode(ViewDocument.self, from: JSONSerialization.data(withJSONObject: object))
    }

    private func form(_ document: ViewDocument) throws -> WireForm {
        try XCTUnwrap(document.sections.flatMap(\.items).compactMap { if case let .form(form) = $0 { form } else { nil } }.first)
    }

    func testGenericModelFormKeepsEditedSubstitutionAndDoesNotPinOnModelOnlyChanges() async throws {
        let form = try self.form(document(version: "one"))
        var initial = ActionFeature.State(dataEpoch: "epoch", client: .ios, lang: "en")
        initial.action = form.submit; initial.fields = form.initialValues; initial.modelFields = form.modelFields
        let store = TestStore(initialState: initial) { ActionFeature() }
        var changed = initial.fields; changed["model"] = .string("codex/precise")
        await store.send(.fieldsChanged(changed)) { $0.fields["model"] = .string("codex/precise"); $0.fields["reasoning_effort"] = .string("auto") }
        XCTAssertEqual(store.state.fields["allow_model_substitution"], .bool(true)); XCTAssertEqual(store.state.fields["pin_account"], .bool(false)); XCTAssertNil(store.state.fields["account_was_edited"])
        changed = store.state.fields; changed["account"] = .string("codex/work")
        await store.send(.fieldsChanged(changed)) { $0.fields["account"] = .string("codex/work"); $0.fields["pin_account"] = .bool(true); $0.fields["account_was_edited"] = .bool(true) }
    }

    func testBackgroundRefreshCannotRebaseAnOpenSelectionDraftOrSealItWithoutExplicitConfirmation() async throws {
        let original = try document(version: "one"), updated = try document(version: "two")
        let old = try form(original)
        var state = ViewsScreenFeature.State(key: .init(rawValue: original.key), owner: "model-editor")
        state.document = original; state.form = old; state.action.dataEpoch = original.dataEpoch
        state.action.action = old.submit; state.action.fields = old.initialValues; state.action.modelFields = old.modelFields
        let replacement = ActionFeature.ChangedNotice(title: String(localized: "This selection changed"), body: String(localized: "Your draft is retained. Save these choices against the current selection?"), actionId: try XCTUnwrap(try form(updated).submit.id), button: String(localized: "Save retained choices"))
        let prepared = LockIsolated<[ActionRequest]>([])
        let store = TestStore(initialState: state) { ViewsScreenFeature() } withDependencies: {
            $0.viewsClient.prepare = { request in prepared.withValue { $0.append(request) }; return request }
        }
        await store.send(.loaded(.document(updated))) { $0.document = updated; $0.action.formReplacement = replacement }
        XCTAssertEqual(store.state.form, old); XCTAssertEqual(store.state.action.fields, old.initialValues)
        await store.send(.action(.submit)) { $0.action.changed = replacement }
        XCTAssertTrue(prepared.value.isEmpty); XCTAssertNil(store.state.action.pending)
        await store.send(.action(.dismissChanged)) { $0.action.changed = nil }
        XCTAssertEqual(store.state.action.fields, old.initialValues)
    }

    func testAnotherDataEpochCannotOfferToRebaseTheDraftOntoAnotherTaskIdentity() async throws {
        let original = try document(version: "one"), updated = try document(version: "two", epoch: "another-domain")
        let old = try form(original)
        var state = ViewsScreenFeature.State(key: .init(rawValue: original.key), owner: "model-editor")
        state.document = original; state.form = old; state.action.dataEpoch = original.dataEpoch
        state.action.action = old.submit; state.action.fields = old.initialValues; state.action.modelFields = old.modelFields
        let store = TestStore(initialState: state) { ViewsScreenFeature() }
        await store.send(.loaded(.document(updated))) { $0.document = updated; $0.action.dataEpoch = updated.dataEpoch; $0.action.modelFormAvailable = false }
        await store.send(.action(.submit)) { $0.action.error = String(localized: "Unavailable in the current catalog") }
        XCTAssertNil(store.state.action.formReplacement); XCTAssertNil(store.state.action.pending); XCTAssertEqual(store.state.action.fields, old.initialValues)
    }
}
