import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import XCTest

@MainActor
final class MachineSettingsFeatureTests: XCTestCase {
    private func store(reply: @escaping @Sendable () async throws -> ActionResponse,
                       requests: LockIsolated<[ActionRequest]>) -> TestStoreOf<MachineSettingsFeature> {
        let store = TestStore(initialState: MachineSettingsFeature.State(client: .ios, lang: "es")) { MachineSettingsFeature() } withDependencies: {
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
        let settings = MachineSetting.all(in: try Fixtures.document("machine-settings-ios-es"))
        XCTAssertEqual(settings.map(\.kind), [.mergeMethod, .branchUpdate, .nodeLimit], "in the daemon's order")
        let setting = try XCTUnwrap(settings.last)
        XCTAssertEqual(setting.title, "Nodos a la vez")
        XCTAssertEqual(setting.detail, "Límite: automático (6)")
        XCTAssertEqual(setting.action.options.map(\.id), ["auto"] + (1...12).map(String.init))
        XCTAssertEqual(setting.action.options.first?.label, "Automático (6) · ahora")

        let mac = try XCTUnwrap(MachineSetting.all(in: try Fixtures.document("machine-settings-mac")).last)
        XCTAssertEqual(mac.detail, "Limit: automatic (6)")
        XCTAssertEqual(MachineSetting.all(in: try Fixtures.document("tasks-ios-es")), [], "the tasks list does not offer them")
    }

    func testAChosenLimitShowsAtOnceAndGoesToTheDaemon() async throws {
        let gate = Gate()
        let requests = LockIsolated<[ActionRequest]>([])
        let store = store(reply: {
            await gate.wait()
            return try Fixtures.response(#"{"protocol":1,"outcome":"applied","message":"Límite actualizado"}"#)
        }, requests: requests)
        await store.send(.loaded(try Fixtures.document("machine-settings-ios-es")))
        let setting = try XCTUnwrap(store.state[.nodeLimit])
        let four = try XCTUnwrap(setting.action.options.first { $0.id == "4" })

        await store.send(.choose(.nodeLimit, four))
        XCTAssertEqual(store.state.detail(.nodeLimit), String(localized: "Nodes at once: 4"), "it shows before the daemon answers")
        XCTAssertEqual(store.state.sending, [.nodeLimit])
        await gate.open()
        await store.receive(\.answered)

        XCTAssertEqual(store.state.sending, [])
        XCTAssertEqual(store.state.errors, [:])
        XCTAssertEqual(store.state.detail(.nodeLimit), String(localized: "Nodes at once: 4"), "kept until the Mac's view says otherwise")
        XCTAssertEqual(requests.value.map(\.actionId), [setting.action.id])
        XCTAssertEqual(requests.value.map(\.optionId), ["4"])
        XCTAssertEqual(requests.value.map(\.client), [.ios])

        await store.send(.loaded(try Fixtures.document("machine-settings-ios-es")))
        XCTAssertEqual(store.state.detail(.nodeLimit), "Límite: automático (6)", "the daemon's view has the last word")
    }

    func testARefusedLimitGoesBackWithTheDaemonsReason() async throws {
        let requests = LockIsolated<[ActionRequest]>([])
        let store = store(reply: {
            try Fixtures.response(#"{"protocol":1,"outcome":"rejected","message":"options.toml tiene un error: arréglalo antes"}"#)
        }, requests: requests)
        await store.send(.loaded(try Fixtures.document("machine-settings-ios-es")))
        let two = try XCTUnwrap(store.state[.nodeLimit]?.action.options.first { $0.id == "2" })

        await store.send(.choose(.nodeLimit, two))
        await store.receive(\.answered)

        XCTAssertEqual(store.state.detail(.nodeLimit), "Límite: automático (6)")
        XCTAssertEqual(store.state.errors[.nodeLimit], "options.toml tiene un error: arréglalo antes")
    }

    func testAnUnreachableMacPutsTheLimitBack() async throws {
        struct Offline: Error {}
        let requests = LockIsolated<[ActionRequest]>([])
        let store = store(reply: { throw Offline() }, requests: requests)
        await store.send(.loaded(try Fixtures.document("machine-settings-ios-es")))
        let two = try XCTUnwrap(store.state[.nodeLimit]?.action.options.first { $0.id == "2" })

        await store.send(.choose(.nodeLimit, two))
        await store.receive(\.failed)

        XCTAssertEqual(store.state.detail(.nodeLimit), "Límite: automático (6)")
        XCTAssertEqual(store.state.errors[.nodeLimit], String(localized: "Your Mac didn’t get the new limit. Try again."))
    }

    func testReadsTheMergeMethodThisMacOffersAboveTheLimit() throws {
        let merge = try XCTUnwrap(MachineSetting.all(in: try Fixtures.document("machine-settings-ios-es")).first)
        XCTAssertEqual(merge.kind, .mergeMethod)
        XCTAssertEqual(merge.title, "Merge de las PR")
        XCTAssertEqual(merge.detail, "Squash and merge · si el repositorio lo permite")
        XCTAssertEqual(merge.action.options.map(\.id), ["squash", "merge", "rebase"])
        XCTAssertEqual(merge.action.options.map(\.description), ["Un solo commit en la rama base", "Conserva todos los commits",
                                                                 "Los commits se reaplican sobre la rama base"])
    }

    func testReadsHowThisMacUpdatesAPRsBranchBelowTheMergeMethod() throws {
        let update = try XCTUnwrap(MachineSetting.all(in: try Fixtures.document("machine-settings-ios-es")).dropFirst().first)
        XCTAssertEqual(update.kind, .branchUpdate)
        XCTAssertEqual(update.title, "Actualizar la rama de un PR con main")
        XCTAssertEqual(update.detail, "Rebase · cuando la PR tiene conflictos")
        XCTAssertEqual(update.action.options.map(\.id), ["rebase", "merge"])
        XCTAssertEqual(update.action.options.map(\.label), ["Rebase · ahora", "Merge"])
    }

    func testAChosenBranchUpdateShowsAtOnceAndGoesToTheDaemon() async throws {
        let requests = LockIsolated<[ActionRequest]>([])
        let store = store(reply: { try Fixtures.response(#"{"protocol":1,"outcome":"applied"}"#) }, requests: requests)
        await store.send(.loaded(try Fixtures.document("machine-settings-ios-es")))
        let update = try XCTUnwrap(store.state[.branchUpdate])
        let merge = try XCTUnwrap(update.action.options.first { $0.id == "merge" })

        await store.send(.choose(.branchUpdate, merge))
        XCTAssertEqual(store.state.detail(.branchUpdate), "Merge", "it shows before the daemon answers")
        XCTAssertEqual(store.state.detail(.mergeMethod), "Squash and merge · si el repositorio lo permite", "the other rows stay as they are")
        await store.receive(\.answered)

        XCTAssertEqual(store.state.errors, [:])
        XCTAssertEqual(requests.value.map(\.actionId), [update.action.id])
        XCTAssertEqual(requests.value.map(\.optionId), ["merge"])
    }

    func testAChosenMergeMethodShowsAtOnceAndGoesToTheDaemonAlone() async throws {
        let gate = Gate()
        let requests = LockIsolated<[ActionRequest]>([])
        let store = store(reply: {
            await gate.wait()
            return try Fixtures.response(#"{"protocol":1,"outcome":"applied","message":"Modo de merge cambiado"}"#)
        }, requests: requests)
        await store.send(.loaded(try Fixtures.document("machine-settings-ios-es")))
        let merge = try XCTUnwrap(store.state[.mergeMethod])
        let rebase = try XCTUnwrap(merge.action.options.first { $0.id == "rebase" })

        await store.send(.choose(.mergeMethod, rebase))
        XCTAssertEqual(store.state.detail(.mergeMethod), "Rebase and merge", "it shows before the daemon answers")
        XCTAssertEqual(store.state.detail(.nodeLimit), "Límite: automático (6)", "the other row stays as it is")
        XCTAssertEqual(store.state.sending, [.mergeMethod])
        await gate.open()
        await store.receive(\.answered)

        XCTAssertEqual(store.state.sending, [])
        XCTAssertEqual(store.state.errors, [:])
        XCTAssertEqual(requests.value.map(\.actionId), [merge.action.id])
        XCTAssertEqual(requests.value.map(\.optionId), ["rebase"])
    }

    func testChoosingTheCurrentMergeMethodShowsItWithoutItsNowMark() async throws {
        let requests = LockIsolated<[ActionRequest]>([])
        let store = store(reply: { try Fixtures.response(#"{"protocol":1,"outcome":"applied"}"#) }, requests: requests)
        await store.send(.loaded(try Fixtures.document("machine-settings-ios-es")))
        let squash = try XCTUnwrap(store.state[.mergeMethod]?.action.options.first { $0.id == "squash" })
        XCTAssertEqual(squash.label, "Squash and merge · ahora")

        await store.send(.choose(.mergeMethod, squash))
        XCTAssertEqual(store.state.detail(.mergeMethod), "Squash and merge")
    }

    func testARefusedMergeMethodGoesBackWithTheDaemonsReason() async throws {
        let requests = LockIsolated<[ActionRequest]>([])
        let store = store(reply: {
            try Fixtures.response(#"{"protocol":1,"outcome":"rejected","message":"Ese modo de merge no existe"}"#)
        }, requests: requests)
        await store.send(.loaded(try Fixtures.document("machine-settings-ios-es")))
        let merge = try XCTUnwrap(store.state[.mergeMethod]?.action.options.first { $0.id == "merge" })

        await store.send(.choose(.mergeMethod, merge))
        await store.receive(\.answered)

        XCTAssertEqual(store.state.detail(.mergeMethod), "Squash and merge · si el repositorio lo permite")
        XCTAssertEqual(store.state.errors[.mergeMethod], "Ese modo de merge no existe")
        XCTAssertNil(store.state.errors[.nodeLimit])
    }

    func testAnUnreachableMacPutsTheMergeMethodBack() async throws {
        struct Offline: Error {}
        let requests = LockIsolated<[ActionRequest]>([])
        let store = store(reply: { throw Offline() }, requests: requests)
        await store.send(.loaded(try Fixtures.document("machine-settings-ios-es")))
        let rebase = try XCTUnwrap(store.state[.mergeMethod]?.action.options.first { $0.id == "rebase" })

        await store.send(.choose(.mergeMethod, rebase))
        await store.receive(\.failed)

        XCTAssertEqual(store.state.detail(.mergeMethod), "Squash and merge · si el repositorio lo permite")
        XCTAssertEqual(store.state.errors[.mergeMethod], String(localized: "Your Mac didn’t get the new merge method. Try again."))
    }
}
