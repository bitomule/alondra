import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI
import XCTest

/// Settings › Claude accounts as the daemon serves it to the iPhone.
@MainActor
final class ClaudeAccountsSnapshotTests: XCTestCase {
    private func settings(_ document: ViewDocument) -> some View {
        var state = ServerSettingsFeature.State(key: .accounts)
        state.document = document
        let store = Store(initialState: state) { ServerSettingsFeature() }
        return Form { ServerSettingsSection(store: store) }
            .scrollContentBackground(.hidden)
            .background(DawnBackground())
            .navigationTitle("Settings")
            .navigationBarTitleDisplayMode(.inline)
    }

    func test_accounts() throws {
        let document = try SampleViews.document("accounts-ios")
        assertScreen([.light, .dark]) { ScreenFrame(height: 1200) { settings(document) } }
    }

    func test_accounts_spanish() throws {
        let document = try SampleViews.document("accounts-ios-es")
        assertScreen([.light], language: .spanish) { ScreenFrame(height: 1200) { settings(document) } }
    }

    func test_codexAccounts() throws {
        let document = try SampleViews.document("accounts-codex-ios")
        assertScreen([.light, .dark]) { ScreenFrame(height: 1200) { settings(document) } }
    }

    func test_codexAccounts_spanish() throws {
        let document = try SampleViews.document("accounts-codex-ios-es")
        assertScreen([.light, .dark], language: .spanish) { ScreenFrame(height: 1200) { settings(document) } }
    }

    /// The settings of a named Antigravity account, opened from the list, with its key section.
    private func keyDetail(_ name: String, account: String, send: CredentialSend? = nil) throws -> some View {
        let document = try SampleViews.document(name)
        let row = try XCTUnwrap(document.sections.flatMap(\.items).compactMap { item -> AlondraContract.Row? in
            if case let .row(row) = item, row.account?.key.account == account { row } else { nil }
        }.first)
        return AccountDetailView(row: row, onAction: { _ in }, onLink: { _ in }, onOption: { _, _ in },
            keyField: AccountKeyField(draft: .constant(""), send: send, onSave: {}, onRemove: {}))
    }

    func test_antigravityKeySet() throws {
        let detail = try keyDetail("accounts-antigravity-ios", account: "studio")
        assertScreen([.light, .dark]) { ScreenFrame(height: 1000) { detail } }
    }

    func test_antigravityNoKey() throws {
        let detail = try keyDetail("accounts-antigravity-ios", account: "fresh")
        assertScreen([.light, .dark]) { ScreenFrame(height: 1000) { detail } }
    }

    func test_antigravityKeySaved_checkResult() throws {
        let detail = try keyDetail("accounts-antigravity-ios", account: "studio", send: CredentialSend(phase: .done("Key saved"), removing: false))
        assertScreen([.light]) { ScreenFrame(height: 1000) { detail } }
    }

    func test_antigravityKey_spanish() throws {
        let set = try keyDetail("accounts-antigravity-ios-es", account: "studio")
        assertScreen([.light, .dark], language: .spanish) { ScreenFrame(height: 1000) { set } }
    }

    func test_antigravityNoKey_spanish() throws {
        let unset = try keyDetail("accounts-antigravity-ios-es", account: "lab")
        assertScreen([.light, .dark], language: .spanish) { ScreenFrame(height: 1000) { unset } }
    }

    func test_antigravityKeySaved_checkResult_spanish() throws {
        let detail = try keyDetail("accounts-antigravity-ios-es", account: "studio", send: CredentialSend(phase: .done("Clave guardada"), removing: false))
        assertScreen([.light], language: .spanish) { ScreenFrame(height: 1000) { detail } }
    }

}
