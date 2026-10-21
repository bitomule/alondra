import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import XCTest

@MainActor
final class ServerSettingsFeatureTests: XCTestCase {
    private func store(reconnected: LockIsolated<[String]> = LockIsolated([])) -> TestStoreOf<ServerSettingsFeature> {
        let store = TestStore(initialState: ServerSettingsFeature.State(key: .mcpServers, client: .ios, lang: "es")) { ServerSettingsFeature() } withDependencies: {
            $0.uuid = .incrementing
            $0.mcpSignIn.reconnect = { target in reconnected.withValue { $0.append(target) } }
        }
        store.exhaustivity = .off
        return store
    }

    private func row(_ document: ViewDocument, _ section: String, _ title: String) throws -> AlondraContract.Row {
        let items = try XCTUnwrap(document.sections.first { $0.id == section }).items
        return try XCTUnwrap(items.compactMap { item -> AlondraContract.Row? in if case .row(let row) = item { row } else { nil } }.first { $0.title == title })
    }

    func testTheDaemonsViewIsShownAsItComesAndItsActionsGoThroughTheGenericActions() async throws {
        let document = try Fixtures.document("mcp-servers-ios-es")
        let store = store()
        await store.send(.loaded(document))
        XCTAssertEqual(store.state.document, document)
        XCTAssertEqual(store.state.actions.dataEpoch, document.dataEpoch)
        let signOut = try XCTUnwrap(try row(document, "yours", "github").actions.first { $0.confirm != nil })
        await store.send(.perform(signOut))
        await store.receive(\.actions.present)
        await store.receive(\.actions.submit)
        XCTAssertEqual(store.state.actions.confirmation, signOut.confirm, "the daemon's confirmation is asked before anything is sent")
    }

    func testConnectOpensTheSignInHereAndReconnectAsksTheMac() async throws {
        let document = try Fixtures.document("mcp-servers-ios-es")
        let reconnected = LockIsolated<[String]>([])
        let store = store(reconnected: reconnected)
        await store.send(.loaded(document))
        let linear = try row(document, "yours", "linear")
        let connect = try XCTUnwrap(linear.actions.first?.link)
        await store.send(.follow(connect))
        XCTAssertEqual(store.state.signIn?.card, linear.id, "the sign-in opens in Settings, on the server's target")
        let reconnect = try XCTUnwrap(try row(document, "yours", "github").actions.first?.link)
        await store.send(.follow(reconnect))
        await store.receive(\.reconnected)
        XCTAssertEqual(reconnected.value.count, 1)
        XCTAssertNotNil(store.state.notice)
    }
}
