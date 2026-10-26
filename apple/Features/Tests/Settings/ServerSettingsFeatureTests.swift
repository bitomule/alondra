import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import XCTest

@MainActor
final class ServerSettingsFeatureTests: XCTestCase {
    private func store(reconnected: LockIsolated<[String]> = LockIsolated([]),
                       reply: @escaping @Sendable () async throws -> ActionResponse = { try Responses.applied() }) -> TestStoreOf<ServerSettingsFeature> {
        let store = TestStore(initialState: ServerSettingsFeature.State(key: .mcpServers, client: .ios, lang: "es")) { ServerSettingsFeature() } withDependencies: {
            $0.uuid = .incrementing
            $0.mcpSignIn.reconnect = { target in reconnected.withValue { $0.append(target) } }
            $0.viewsClient.perform = { _ in try await reply() }
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

    private func deleteAction(_ document: ViewDocument) throws -> (AlondraContract.Row, WireAction) {
        let github = try row(document, "yours", "github")
        return (github, try XCTUnwrap(github.actions.first { $0.resolvesCard == true }))
    }

    func testDeletingAServerHidesItsRowOnceConfirmedAndKeepsItHiddenWhenTheMacDeletesIt() async throws {
        let document = try Fixtures.document("mcp-servers-ios-es")
        let gate = Gate()
        let store = store(reply: {
            await gate.wait()
            return try Responses.applied("Borrado")
        })
        await store.send(.loaded(document))
        let (github, delete) = try deleteAction(document)
        await store.send(.perform(delete))
        await store.receive(\.actions.submit)
        XCTAssertTrue(store.state.hiddenRows.isEmpty, "nothing goes while the confirmation is open")

        await store.send(.actions(.confirm))
        await store.receive(\.actions.sending)
        XCTAssertEqual(store.state.hiddenRows, [github.id], "the row goes before the Mac answers")
        await gate.open()
        await store.receive(\.actions.answered)
        XCTAssertEqual(store.state.hiddenRows, [github.id], "kept hidden until the Mac's view drops it")
        XCTAssertNil(store.state.error ?? store.state.actions.error)

        await store.send(.loaded(document))
        XCTAssertEqual(store.state.hiddenRows, [github.id], "a view from before the delete does not bring it back")
    }

    func testAFailedDeleteBringsTheRowBackWithTheMacsReason() async throws {
        let document = try Fixtures.document("mcp-servers-ios-es")
        let store = store(reply: {
            try Fixtures.response(#"{"protocol":1,"outcome":"rejected","message":"Claude Code no pudo borrar github"}"#)
        })
        await store.send(.loaded(document))
        let (_, delete) = try deleteAction(document)
        await store.send(.perform(delete))
        await store.receive(\.actions.submit)
        await store.send(.actions(.confirm))
        await store.receive(\.actions.delegate.failed)
        XCTAssertTrue(store.state.hiddenRows.isEmpty, "the row is back")
        XCTAssertEqual(store.state.error, "Claude Code no pudo borrar github")
    }

    func testAnActionThatKeepsItsRowNeverHidesIt() async throws {
        let document = try Fixtures.document("mcp-servers-ios-es")
        let store = store()
        await store.send(.loaded(document))
        let check = try XCTUnwrap(document.sections.flatMap(\.items).compactMap { item -> WireAction? in
            guard case .row(let row) = item else { return nil }
            return row.actions.first { $0.resolvesCard == false && $0.confirm == nil && $0.link == nil }
        }.first)
        await store.send(.perform(check))
        await store.receive(\.actions.sending)
        XCTAssertTrue(store.state.hiddenRows.isEmpty)
    }
}
