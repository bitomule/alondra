import AlondraContract
import AlondraFeatures
import ComposableArchitecture
import XCTest

@MainActor
final class CodexAccountsFeatureTests: XCTestCase {
    func testCodexLoginUsesTheValidatedProviderArgumentsAndKeepsTheProviderInTheSheet() async {
        let ran = LockIsolated<[String]>([])
        let store = TestStore(initialState: ServerSettingsFeature.State(key: .accounts, client: .ios, lang: "en")) { ServerSettingsFeature() } withDependencies: {
            $0.claudeTerminal.open = { command in ran.withValue { $0.append(command) }; return false }
        }
        let arguments = ["accounts", "login", "--provider", "codex", "work"]
        await store.send(.follow(.local(local: .accountSignIn(account: "work", command: "legacy command must not run", provider: .codex, argv: arguments))))
        let command = "'alondra' 'accounts' 'login' '--provider' 'codex' 'work'"
        await store.receive(\.terminalAnswered) {
            $0.claudeSignIn = ServerSettingsFeature.ClaudeSignIn(command: command, terminalOpen: false, provider: .codex)
        }
        XCTAssertEqual(ran.value, [command])
    }

    func testProviderMismatchIsVisibleAndNeverStartsTerminal() async {
        let ran = LockIsolated<[String]>([])
        let store = TestStore(initialState: ServerSettingsFeature.State(key: .accounts, client: .ios, lang: "en")) { ServerSettingsFeature() } withDependencies: {
            $0.claudeTerminal.open = { command in ran.withValue { $0.append(command) }; return false }
        }
        await store.send(.follow(.local(local: .accountSignIn(account: "work", command: "alondra accounts login work", provider: .codex, argv: ["accounts", "login", "--provider", "claude", "work"])))) {
            $0.error = String(localized: "This account action is unavailable. Update Alondra and try again.")
        }
        XCTAssertTrue(ran.value.isEmpty)
    }

    func testAMismatchedCodexAccountSlugCannotOpenAnotherAccountLogin() async {
        let ran = LockIsolated<[String]>([])
        let store = TestStore(initialState: ServerSettingsFeature.State(key: .accounts, client: .ios, lang: "en")) { ServerSettingsFeature() } withDependencies: {
            $0.claudeTerminal.open = { command in ran.withValue { $0.append(command) }; return false }
        }
        await store.send(.follow(.local(local: .accountSignIn(account: "work", command: "legacy", provider: .codex, argv: ["accounts", "login", "--provider", "codex", "personal"])))) {
            $0.error = String(localized: "This account action is unavailable. Update Alondra and try again.")
        }
        XCTAssertTrue(ran.value.isEmpty)
    }

    func testCodexAccountNameIsAddedToTheSameProviderCommand() async {
        let ran = LockIsolated<[String]>([])
        let store = TestStore(initialState: ServerSettingsFeature.State(key: .accounts, client: .ios, lang: "en")) { ServerSettingsFeature() } withDependencies: {
            $0.claudeTerminal.open = { command in ran.withValue { $0.append(command) }; return true }
        }
        let arguments = ["accounts", "add", "--provider", "codex"]
        await store.send(.follow(.local(local: .accountAdd(command: "ignored", provider: .codex, argv: arguments)))) {
            $0.newAccount = ServerSettingsFeature.NewAccount(command: "ignored", provider: .codex, argv: arguments)
        }
        await store.send(.newAccountNameChanged("Personal")) { $0.newAccount?.name = "Personal" }
        await store.send(.newAccountAdded) { $0.newAccount = nil }
        let command = "'alondra' 'accounts' 'add' '--provider' 'codex' '--' 'Personal'"
        await store.receive(\.terminalAnswered) {
            $0.claudeSignIn = ServerSettingsFeature.ClaudeSignIn(command: command, terminalOpen: true, provider: .codex)
        }
        XCTAssertEqual(ran.value, [command])
    }
}
