import AlondraContract
import AlondraFeatures
import AlondraFeed
import AlondraiOSUI
import ComposableArchitecture
import SwiftUI
import XCTest

/// The New tab: an empty composer, a drafted request with its options, the queue with requests on
/// their way and one that was not sent, the composer before the options arrive, the options sheet,
/// and the issue picker with a pasted link offered as a row.
@MainActor
final class NewRequestSnapshotTests: XCTestCase {
    override func setUp() async throws { ScreenStores.pinTime() }

    private func composer(_ sample: String = "new-request-ios", edit: (inout NewRequestFeature.State) -> Void = { _ in })
        throws -> StoreOf<NewRequestFeature> {
        let document = try SampleViews.document(sample)
        var state = NewRequestFeature.State()
        state.screen = .showing(document, key: .newRequest())
        for field in state.form?.fields ?? [] { state.values[field.id] = field.value }
        edit(&state)
        return Store(initialState: state) { NewRequestFeature() } withDependencies: {
            $0.viewsClient.observe = { _ in AsyncThrowingStream { _ in } }
        }
    }

    private func options(_ sample: String = "new-request-ios", edit: (inout NewRequestFeature.State) -> Void = { _ in })
        throws -> StoreOf<RequestOptionsFeature> {
        let state = try composer(sample, edit: edit).withState { $0 }
        return Store(initialState: RequestOptionsFeature.State(fields: state.optionFields, values: state.values)) {
            RequestOptionsFeature()
        }
    }

    private func drafted(_ state: inout NewRequestFeature.State) {
        state.values["text"] = .string("Polish the Alondra beta before Friday: fix the onboarding copy and the empty states.")
        state.values["project"] = .string("Demo Garden")
        state.values["issue"] = .string("me/Garden#12")
    }

    func test_empty() throws {
        let store = try composer()
        assertScreen([.light, .dark], inWindow: true) { ScreenFrame(height: 852) { NewRequestView(store: store) } }
    }

    func test_empty_spanish() throws {
        let store = try composer("new-request-ios-es")
        assertScreen([.light], language: .spanish, inWindow: true) { ScreenFrame(height: 852) { NewRequestView(store: store) } }
    }

    func test_empty_accessibilitySize() throws {
        let store = try composer()
        assertScreen([.ax3], inWindow: true) { ScreenFrame(height: 852) { NewRequestView(store: store) } }
    }

    func test_drafted() throws {
        let store = try composer(edit: drafted)
        assertScreen([.light, .dark], inWindow: true) { ScreenFrame(height: 852) { NewRequestView(store: store) } }
    }

    private func queued(_ texts: (sending: String, unconfirmed: String, failed: String, reason: String)) -> (inout NewRequestFeature.State) -> Void {
        { state in
            state.pending.add("r-failed", text: texts.failed, phase: .failed(texts.reason))
            state.pending.add("r-unconfirmed", text: texts.unconfirmed, phase: .unconfirmed)
            state.pending.add("r-sending", text: texts.sending, phase: .sending)
        }
    }

    func test_queue() throws {
        let store = try composer(edit: queued((
            sending: "Polish the Alondra beta before Friday: fix the onboarding copy and the empty states.",
            unconfirmed: "Add a widget with the queue",
            failed: "Rename the Garden project to Demo Garden",
            reason: "Alondra can’t take requests right now. Try again in a moment."
        )))
        assertScreen([.light, .dark], inWindow: true) { ScreenFrame(height: 1100) { NewRequestView(store: store) } }
    }

    func test_queue_spanish() throws {
        let store = try composer("new-request-ios-es", edit: queued((
            sending: "Pule la beta de Alondra antes del viernes: el texto de bienvenida y las pantallas vacías.",
            unconfirmed: "Añade un widget con la cola",
            failed: "Cambia el nombre del proyecto Garden a Demo Garden",
            reason: "Alondra no puede aceptar solicitudes ahora. Vuelve a intentarlo en un momento."
        )))
        assertScreen([.light], language: .spanish, inWindow: true) { ScreenFrame(height: 1100) { NewRequestView(store: store) } }
    }

    /// The request this iPhone sent, on its way, and the daemon's row for it, named by the
    /// operation that carried it: the queue shows it once.
    func test_sentAndShownByTheDaemon_spanish() throws {
        let nonce = "27d86775-cd69-4956-92d0-4a20cd01c813"
        let text = "Bug para el nodo de Redraft. Tenía todas las fotos analizadas, salí de casa y me saltó la alerta de tienes x fotos por analizar."
        var document = try XCTUnwrap(JSONSerialization.jsonObject(
            with: ContractCodec.encode(SampleViews.document("new-request-ios-es"))) as? [String: Any])
        var sections = try XCTUnwrap(document["sections"] as? [[String: Any]])
        let sent = try XCTUnwrap(sections.firstIndex { $0["id"] as? String == "sent" })
        var row = try XCTUnwrap((sections[sent]["items"] as? [[String: Any]])?.first)
        row["id"] = "op_8e8dd8f1687d492b5a16fb35d2ca17cd_1_85_\(nonce)"
        row["title"] = String(text.prefix(100))
        sections[sent]["items"] = [row]
        document["sections"] = sections
        let shown = try ContractCodec.decode(ViewDocument.self, from: JSONSerialization.data(withJSONObject: document))
        var state = NewRequestFeature.State()
        state.screen = .showing(shown, key: .newRequest())
        state.pending.add(nonce, text: text, phase: .sending)
        let store = Store(initialState: state) { NewRequestFeature() } withDependencies: {
            $0.viewsClient.observe = { _ in AsyncThrowingStream { _ in } }
        }
        assertScreen([.light], language: .spanish, inWindow: true) { ScreenFrame(height: 852) { NewRequestView(store: store) } }
    }

    func test_beforeTheOptionsArrive() throws {
        var state = NewRequestFeature.State()
        state.values["text"] = .string("Polish the Alondra beta before Friday.")
        state.pending.add("r-waiting", text: "Add a widget with the queue", phase: .waiting(.options))
        let store = Store(initialState: state) { NewRequestFeature() } withDependencies: {
            $0.viewsClient.observe = { _ in AsyncThrowingStream { _ in } }
        }
        assertScreen([.light], inWindow: true) { ScreenFrame(height: 852) { NewRequestView(store: store) } }
    }

    func test_optionsSheet() throws {
        let store = try options(edit: drafted)
        assertScreen([.light, .dark], inWindow: true) { ScreenFrame(height: 700) { RequestOptionsSheet(store: store) } }
    }

    func test_optionsSheet_spanish() throws {
        let store = try options("new-request-ios-es")
        assertScreen([.light], language: .spanish, inWindow: true) { ScreenFrame(height: 700) { RequestOptionsSheet(store: store) } }
    }

    func test_issuePicker_withAPastedLink() throws {
        var state = IssuePickerFeature.State(selected: "")
        state.query = "https://github.com/me/Other/issues/3"
        state.loading = false
        let document = try SampleViews.document("issue-picker")
        state.rows = document.sections.flatMap(\.items).compactMap { if case .row(let row) = $0 { row } else { nil } }
        let store = Store(initialState: state) { EmptyReducer<IssuePickerFeature.State, IssuePickerFeature.Action>() }
        assertScreen([.light, .dark], inWindow: true) { ScreenFrame(height: 700) { IssuePickerView(store: store) } }
    }
    func test_codexModelOptions() throws {
        let store = try options("new-request-codex-ios") { state in
            state.values["model"] = .string("codex/fixture-general")
            state.values["reasoning_effort"] = .string("high")
            state.values["allow_model_substitution"] = .bool(false)
        }
        assertScreen([.light, .dark], inWindow: true) { ScreenFrame(height: 852) { RequestOptionsSheet(store: store) } }
    }

    func test_codexModelOptions_spanish() throws {
        let store = try options("new-request-codex-ios-es") { state in
            state.values["model"] = .string("codex/fixture-general")
            state.values["reasoning_effort"] = .string("high")
            state.values["allow_model_substitution"] = .bool(false)
        }
        assertScreen([.light], language: .spanish, inWindow: true) { ScreenFrame(height: 852) { RequestOptionsSheet(store: store) } }
    }

}
