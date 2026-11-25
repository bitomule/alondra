import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import XCTest

/// The key field of an Antigravity account's settings: the key goes once, inside the action, and
/// nothing on the screen keeps it or shows it back.
@MainActor
final class AccountKeyFeatureTests: XCTestCase {
    private let key = "AIzaSyD-example-0000000000000000000000"

    private func store(sent: LockIsolated<[ActionRequest]>, reply: @escaping @Sendable () async throws -> ActionResponse = { try Responses.applied("Key saved") })
        throws -> TestStoreOf<ServerSettingsFeature> {
        let document = try Fixtures.document("accounts-antigravity-ios")
        var state = ServerSettingsFeature.State(key: .accounts, client: .ios, lang: "en")
        state.document = document; state.actions.dataEpoch = document.dataEpoch
        let store = TestStore(initialState: state) { ServerSettingsFeature() } withDependencies: {
            $0.uuid = .incrementing
            $0.viewsClient.perform = { request in
                sent.withValue { $0.append(request) }
                return try await reply()
            }
        }
        store.exhaustivity = .off
        return store
    }

    func testTheDaemonSaysWhetherAKeyIsSetAndNeverSendsOneBack() throws {
        let document = try Fixtures.document("accounts-antigravity-ios")
        let accounts = ServerSettingsFeature.State(key: .accounts).with(document).accountRows.compactMap(\.account)
        XCTAssertEqual(accounts.map(\.key.account), ["studio", "fresh", "lab"])
        XCTAssertEqual(accounts.map { $0.credential?.state.text }, ["Key set", "No key", "No key"])
        XCTAssertEqual(accounts.map { $0.credential?.set }, [true, false, false])
        XCTAssertEqual(accounts.map { $0.credential?.label }, ["Gemini API key", "Gemini API key", "Agent Platform key"])
        let spanish = ServerSettingsFeature.State(key: .accounts).with(try Fixtures.document("accounts-antigravity-ios-es")).accountRows.compactMap(\.account)
        XCTAssertEqual(spanish.map { $0.credential?.state.text }, ["Clave puesta", "Sin clave", "Sin clave"])
        let account = try XCTUnwrap(try Fixtures.document("accounts-codex-ios").sections.flatMap(\.items).compactMap { item -> AccountPresentation? in
            if case .row(let row) = item { row.account } else { nil }
        }.first)
        XCTAssertNil(account.credential, "an account that takes no key has no key field")
    }

    func testTheFieldStartsEmptyEveryTimeAnAccountOpens() async throws {
        let sent = LockIsolated<[ActionRequest]>([])
        let store = try store(sent: sent)
        await store.send(.accountOpened("antigravity-studio"))
        XCTAssertEqual(store.state.credentialDraft, "", "a key that is set is never prefilled")
        XCTAssertEqual(store.state.accountDetail?.account?.credential?.set, true)
        await store.send(.credentialDraftChanged(key))
        await store.send(.accountDetailPresented(nil))
        XCTAssertEqual(store.state.credentialDraft, "", "closing the account forgets what was typed")
        await store.send(.accountOpened("antigravity-studio"))
        XCTAssertEqual(store.state.credentialDraft, "")
        XCTAssertTrue(sent.value.isEmpty, "nothing goes until the key is saved")
    }

    func testSavingSendsTheKeyOnceInTheActionAndForgetsItOnTheScreen() async throws {
        let sent = LockIsolated<[ActionRequest]>([])
        let gate = Gate()
        let store = try store(sent: sent, reply: {
            await gate.wait()
            return try Responses.applied("Key saved")
        })
        await store.send(.accountOpened("antigravity-fresh"))
        await store.send(.credentialDraftChanged("  \(key) "))
        await store.send(.credentialSaved)
        await store.receive(\.credentialSent)
        XCTAssertEqual(store.state.credentialDraft, "")
        XCTAssertEqual(store.state.actions.text, "", "the field is emptied the moment the key is sent")
        XCTAssertEqual(store.state.credentialSend?.phase, .sending)
        XCTAssertEqual(store.state.accountDetail?.id, "antigravity-fresh", "the account stays open to show the result")
        await store.receive(\.actions.sending)
        await gate.open()
        await store.receive(\.actions.answered)
        await store.receive(\.actions.response)
        XCTAssertEqual(store.state.credentialSend?.phase, .done("Key saved"))
        let request = try XCTUnwrap(sent.value.first)
        XCTAssertEqual(sent.value.count, 1)
        XCTAssertTrue(request.actionId.hasPrefix("account_credential:fresh@"), request.actionId)
        XCTAssertNil(request.optionId)
        XCTAssertEqual(request.input.text, key, "the key travels only as the action's text")
        XCTAssertFalse(String(describing: store.state).contains(key), "once answered nothing on the screen holds the key")
    }

    func testRemovingSendsTheClearOptionWithoutAKey() async throws {
        let sent = LockIsolated<[ActionRequest]>([])
        let store = try store(sent: sent, reply: { try Responses.applied("Key removed") })
        await store.send(.accountOpened("antigravity-studio"))
        await store.send(.credentialDraftChanged(key))
        await store.send(.credentialRemoved)
        await store.receive(\.credentialSent)
        await store.receive(\.actions.answered)
        XCTAssertEqual(store.state.credentialSend, CredentialSend(phase: .done("Key removed"), removing: true, commandNonce: store.state.credentialSend?.commandNonce))
        let request = try XCTUnwrap(sent.value.first)
        XCTAssertEqual(request.optionId, "clear")
        XCTAssertNil(request.input.text, "removing a key sends no key")
        XCTAssertEqual(store.state.credentialDraft, "")
    }

    func testARefusedKeyShowsTheDaemonsReasonAndKeepsNothing() async throws {
        let sent = LockIsolated<[ActionRequest]>([])
        let store = try store(sent: sent, reply: { try Responses.refused("That is not a Gemini API key") })
        await store.send(.accountOpened("antigravity-fresh"))
        await store.send(.credentialDraftChanged("not-a-key"))
        await store.send(.credentialSaved)
        await store.receive(\.actions.answered)
        XCTAssertEqual(store.state.credentialSend?.phase, .failed("That is not a Gemini API key"))
        XCTAssertEqual(store.state.credentialDraft, "", "a refused key is typed again, never kept to retry")
    }

    func testAnAccountWithoutAKeyFieldSendsNothing() async throws {
        let sent = LockIsolated<[ActionRequest]>([])
        let store = try store(sent: sent)
        await store.send(.credentialDraftChanged(key))
        await store.send(.credentialSaved)
        XCTAssertNil(store.state.credentialSend)
        XCTAssertTrue(sent.value.isEmpty)
    }
}

private extension ServerSettingsFeature.State {
    func with(_ document: ViewDocument) -> Self {
        var state = self
        state.document = document
        return state
    }
}
