import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import XCTest

@MainActor
final class ClaudeAccountsFeatureTests: XCTestCase {
    func testTheLinksOfDaemonsThatSignAccountsInDoNothing() async {
        let store = TestStore(initialState: ServerSettingsFeature.State(key: .accounts, client: .ios, lang: "en")) { ServerSettingsFeature() }
        await store.send(.follow(.local(local: .accountAdd(command: "alondra accounts add"))))
        await store.send(.follow(.local(local: .accountSignIn(account: "Personal", command: "alondra accounts login 'Personal'"))))
    }

    private func row(_ document: ViewDocument, _ id: String) throws -> AlondraContract.Row {
        let rows = document.sections.flatMap(\.items).compactMap { item -> AlondraContract.Row? in if case .row(let row) = item { row } else { nil } }
        return try XCTUnwrap(rows.first { $0.id == id }, id)
    }

    func testTheDaemonsAccountsScreenPicksAWeightFromItsMenu() async throws {
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

        let weight = try XCTUnwrap(try row(document, "claude-work").actions.first { $0.presentation == .menu })
        await store.send(.performOption(weight, optionID: "10"))
        await store.receive(\.actions.delegate.completed)
        XCTAssertEqual(requests.value.map(\.actionId), [weight.id])
        XCTAssertEqual(requests.value.map(\.optionId), ["10"])
    }
}
