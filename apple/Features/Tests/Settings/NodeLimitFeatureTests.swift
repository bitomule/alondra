import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import XCTest

@MainActor
final class NodeLimitFeatureTests: XCTestCase {
    private func store(reply: @escaping @Sendable () async throws -> ActionResponse,
                       requests: LockIsolated<[ActionRequest]>) -> TestStoreOf<NodeLimitFeature> {
        let store = TestStore(initialState: NodeLimitFeature.State(client: .ios, lang: "es")) { NodeLimitFeature() } withDependencies: {
            $0.uuid = .incrementing
            $0.viewsClient.perform = { request in
                requests.withValue { $0.append(request) }
                return try await reply()
            }
        }
        store.exhaustivity = .off
        return store
    }

    func testReadsTheLimitThisMacOffersInItsSettings() throws {
        let setting = try XCTUnwrap(NodeLimitSetting(document: try Fixtures.document("machine-settings-ios-es")))
        XCTAssertEqual(setting.title, "Nodos a la vez")
        XCTAssertEqual(setting.detail, "Límite: automático (6)")
        XCTAssertEqual(setting.action.options.map(\.id), ["auto"] + (1...12).map(String.init))
        XCTAssertEqual(setting.action.options.first?.label, "Automático (6) · ahora")

        let mac = try XCTUnwrap(NodeLimitSetting(document: try Fixtures.document("machine-settings-mac")))
        XCTAssertEqual(mac.detail, "Limit: automatic (6)")
        XCTAssertNil(NodeLimitSetting(document: try Fixtures.document("tasks-ios-es")), "the tasks list does not offer it")
    }

    func testAChosenLimitShowsAtOnceAndGoesToTheDaemon() async throws {
        let gate = Gate()
        let requests = LockIsolated<[ActionRequest]>([])
        let store = store(reply: {
            await gate.wait()
            return try Fixtures.response(#"{"protocol":1,"outcome":"applied","message":"Límite actualizado"}"#)
        }, requests: requests)
        await store.send(.loaded(try Fixtures.document("machine-settings-ios-es")))
        let setting = try XCTUnwrap(store.state.setting)
        let four = try XCTUnwrap(setting.action.options.first { $0.id == "4" })

        await store.send(.choose(four))
        XCTAssertEqual(store.state.detail, String(localized: "Nodes at once: 4"), "it shows before the daemon answers")
        XCTAssertTrue(store.state.sending)
        await gate.open()
        await store.receive(\.answered)

        XCTAssertFalse(store.state.sending)
        XCTAssertNil(store.state.error)
        XCTAssertEqual(store.state.detail, String(localized: "Nodes at once: 4"), "kept until the Mac's view says otherwise")
        XCTAssertEqual(requests.value.map(\.actionId), [setting.action.id])
        XCTAssertEqual(requests.value.map(\.optionId), ["4"])
        XCTAssertEqual(requests.value.map(\.client), [.ios])

        await store.send(.loaded(try Fixtures.document("machine-settings-ios-es")))
        XCTAssertEqual(store.state.detail, "Límite: automático (6)", "the daemon's view has the last word")
    }

    func testARefusedLimitGoesBackWithTheDaemonsReason() async throws {
        let requests = LockIsolated<[ActionRequest]>([])
        let store = store(reply: {
            try Fixtures.response(#"{"protocol":1,"outcome":"rejected","message":"options.toml tiene un error: arréglalo antes"}"#)
        }, requests: requests)
        await store.send(.loaded(try Fixtures.document("machine-settings-ios-es")))
        let two = try XCTUnwrap(store.state.setting?.action.options.first { $0.id == "2" })

        await store.send(.choose(two))
        await store.receive(\.answered)

        XCTAssertEqual(store.state.detail, "Límite: automático (6)")
        XCTAssertEqual(store.state.error, "options.toml tiene un error: arréglalo antes")
    }

    func testAnUnreachableMacPutsTheLimitBack() async throws {
        struct Offline: Error {}
        let requests = LockIsolated<[ActionRequest]>([])
        let store = store(reply: { throw Offline() }, requests: requests)
        await store.send(.loaded(try Fixtures.document("machine-settings-ios-es")))
        let two = try XCTUnwrap(store.state.setting?.action.options.first { $0.id == "2" })

        await store.send(.choose(two))
        await store.receive(\.failed)

        XCTAssertEqual(store.state.detail, "Límite: automático (6)")
        XCTAssertEqual(store.state.error, String(localized: "Your Mac didn’t get the new limit. Try again."))
    }
}
