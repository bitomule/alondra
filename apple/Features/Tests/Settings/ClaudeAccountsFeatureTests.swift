import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import XCTest

@MainActor
final class ClaudeAccountsFeatureTests: XCTestCase {
    private func store(opened: Bool, ran: LockIsolated<[String]>) -> TestStoreOf<ServerSettingsFeature> {
        TestStore(initialState: ServerSettingsFeature.State(key: .accounts, client: .ios, lang: "en")) { ServerSettingsFeature() } withDependencies: {
            $0.claudeTerminal.open = { command in
                ran.withValue { $0.append(command) }
                return opened
            }
        }
    }

    func testAddingAnAccountAsksItsNameAndSignsItInWithTheNameQuotedForTheShell() async {
        let ran = LockIsolated<[String]>([])
        let store = store(opened: false, ran: ran)
        await store.send(.follow(.local(local: .accountAdd(command: "alondra accounts add")))) {
            $0.newAccount = ServerSettingsFeature.NewAccount(command: "alondra accounts add")
        }
        await store.send(.newAccountNameChanged("  Pat's work ")) { $0.newAccount?.name = "  Pat's work " }
        await store.send(.newAccountAdded) { $0.newAccount = nil }
        let command = #"alondra accounts add 'Pat'\''s work'"#
        await store.receive(\.terminalAnswered) {
            $0.claudeSignIn = ServerSettingsFeature.ClaudeSignIn(command: command, terminalOpen: false)
        }
        XCTAssertEqual(ran.value, [command])
    }

    func testANameOfOnlySpacesAddsNothing() async {
        let ran = LockIsolated<[String]>([])
        let store = store(opened: false, ran: ran)
        await store.send(.follow(.local(local: .accountAdd(command: "alondra accounts add")))) {
            $0.newAccount = ServerSettingsFeature.NewAccount(command: "alondra accounts add")
        }
        await store.send(.newAccountNameChanged("   ")) { $0.newAccount?.name = "   " }
        XCTAssertEqual(store.state.newAccount?.canAdd, false)
        await store.send(.newAccountAdded)
        await store.send(.newAccountCancelled) { $0.newAccount = nil }
        XCTAssertEqual(ran.value, [])
    }

    func testSigningInRunsTheDaemonsCommandAsItCameAndSaysTerminalIsOpenOnThisMac() async {
        let ran = LockIsolated<[String]>([])
        let store = store(opened: true, ran: ran)
        let command = "alondra accounts login 'Personal'"
        await store.send(.follow(.local(local: .accountSignIn(account: "Personal", command: command))))
        await store.receive(\.terminalAnswered) {
            $0.claudeSignIn = ServerSettingsFeature.ClaudeSignIn(command: command, terminalOpen: true)
        }
        XCTAssertEqual(ran.value, [command])
        await store.send(.claudeSignInPresented(nil)) { $0.claudeSignIn = nil }
    }

    private func row(_ document: ViewDocument, _ id: String) throws -> AlondraContract.Row {
        let rows = document.sections.flatMap(\.items).compactMap { item -> AlondraContract.Row? in if case .row(let row) = item { row } else { nil } }
        return try XCTUnwrap(rows.first { $0.id == id }, id)
    }

    func testTheDaemonsAccountsScreenAddsByNameAndPicksAWeightFromItsMenu() async throws {
        let document = try Fixtures.document("accounts-ios")
        let requests = LockIsolated<[ActionRequest]>([])
        let store = TestStore(initialState: ServerSettingsFeature.State(key: .accounts, client: .ios, lang: "en")) { ServerSettingsFeature() } withDependencies: {
            $0.uuid = .incrementing
            $0.viewsClient.perform = { request in
                requests.withValue { $0.append(request) }
                return try Fixtures.response(#"{"protocol":1,"outcome":"applied","message":"Weight set"}"#)
            }
        }
        store.exhaustivity = .off
        await store.send(.loaded(document))

        let add = try XCTUnwrap(try row(document, "claude-add").actions.first?.link)
        await store.send(.follow(add))
        XCTAssertEqual(store.state.newAccount, ServerSettingsFeature.NewAccount(command: "alondra accounts add"))

        let weight = try XCTUnwrap(try row(document, "claude-work").actions.first { $0.presentation == .menu })
        await store.send(.performOption(weight, optionID: "10"))
        await store.receive(\.actions.delegate.completed)
        XCTAssertEqual(requests.value.map(\.actionId), [weight.id])
        XCTAssertEqual(requests.value.map(\.optionId), ["10"])
    }

    func testShellQuotingKeepsEveryCharacterLiteral() {
        XCTAssertEqual(ServerSettingsFeature.shellQuoted("Work"), "'Work'")
        XCTAssertEqual(ServerSettingsFeature.shellQuoted("a'b"), #"'a'\''b'"#)
        XCTAssertEqual(ServerSettingsFeature.shellQuoted("$(rm -rf ~) `x`"), "'$(rm -rf ~) `x`'")
    }
}
