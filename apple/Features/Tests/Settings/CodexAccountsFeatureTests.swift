import AlondraContract
import AlondraFeatures
import ComposableArchitecture
import XCTest

@MainActor
final class CodexAccountsFeatureTests: XCTestCase {
    func testCodexLinksOfDaemonsThatSignAccountsInDoNothing() async {
        let store = TestStore(initialState: ServerSettingsFeature.State(key: .accounts, client: .ios, lang: "en")) { ServerSettingsFeature() }
        let login = ["accounts", "login", "--provider", "codex", "work"]
        await store.send(.follow(.local(local: .accountSignIn(account: "work", command: "alondra accounts login work", provider: .codex, argv: login))))
        let add = ["accounts", "add", "--provider", "codex"]
        await store.send(.follow(.local(local: .accountAdd(command: "alondra accounts add", provider: .codex, argv: add))))
    }
}
