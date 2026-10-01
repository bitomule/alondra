import AlondraContract
import AlondraFeatures
import AlondraFeed
import AlondraiOSUI
import ComposableArchitecture
import SwiftUI
import XCTest

/// The New tab: an empty composer, a drafted request with its options, the options sheet, and the
/// issue picker with a pasted link offered as a row.
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
        let store = try composer("new-request-es")
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

    func test_optionsSheet() throws {
        let store = try options(edit: drafted)
        assertScreen([.light, .dark], inWindow: true) { ScreenFrame(height: 700) { RequestOptionsSheet(store: store) } }
    }

    func test_optionsSheet_spanish() throws {
        let store = try options("new-request-es")
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
}
