import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI
import XCTest

/// Settings › Claude accounts as the daemon serves it to the iPhone, and the sheet that leaves the
/// sign-in to Terminal on the Mac.
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

    private var finishOnTheMac: some View {
        ScreenFrame(height: 420) {
            ClaudeSignInSheet(signIn: .init(command: "alondra accounts login 'Personal'", terminalOpen: false)) {}
        }
    }

    func test_accounts() throws {
        let document = try SampleViews.document("accounts-ios")
        assertScreen([.light, .dark]) { ScreenFrame(height: 1200) { settings(document) } }
    }

    func test_accounts_spanish() throws {
        let document = try SampleViews.document("accounts-ios-es")
        assertScreen([.light], language: .spanish) { ScreenFrame(height: 1200) { settings(document) } }
    }

    func test_finishOnTheMac() {
        assertScreen([.light, .dark], inWindow: true) { finishOnTheMac }
    }

    func test_finishOnTheMac_spanish() {
        assertScreen([.light], language: .spanish, inWindow: true) { finishOnTheMac }
    }
    func test_codexAccounts() throws {
        let document = try SampleViews.document("accounts-codex-ios")
        assertScreen([.light, .dark]) { ScreenFrame(height: 1200) { settings(document) } }
    }

    func test_codexAccounts_spanish() throws {
        let document = try SampleViews.document("accounts-codex-ios-es")
        assertScreen([.light], language: .spanish) { ScreenFrame(height: 1200) { settings(document) } }
    }

}
