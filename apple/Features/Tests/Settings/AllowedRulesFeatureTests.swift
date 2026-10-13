import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import XCTest

@MainActor
final class AllowedRulesFeatureTests: XCTestCase {
    private func store(reply: @escaping @Sendable () async throws -> ActionResponse,
                       requests: LockIsolated<[ActionRequest]>) -> TestStoreOf<AllowedRulesFeature> {
        let store = TestStore(initialState: AllowedRulesFeature.State(client: .ios, lang: "es")) { AllowedRulesFeature() } withDependencies: {
            $0.uuid = .incrementing
            $0.viewsClient.perform = { request in
                requests.withValue { $0.append(request) }
                return try await reply()
            }
        }
        store.exhaustivity = .off
        return store
    }

    func testReadsTheRulesThisMacAllowsAlways() throws {
        let rules = AllowedRule.all(in: try Fixtures.document("machine-settings-ios-es"))
        XCTAssertEqual(rules.map(\.id), ["rule-3", "rule-4"])
        XCTAssertEqual(rules.first?.title,
                       "`make build` con cualquier argumento: ejecuta lo que diga el `Makefile` del repo, también si el nodo lo cambia")
        XCTAssertEqual(rules.first?.detail, "alondra · añadido 29 sept · tarea «Build the importer»")
        XCTAssertEqual(rules.first?.remove.label, "Quitar")
        XCTAssertTrue(AllowedRule.all(in: try Fixtures.document("fleet-ios-es")).isEmpty)
    }

    func testRemovingARuleHidesItAtOnceAndSendsTheDaemonItsAction() async throws {
        let gate = Gate()
        let requests = LockIsolated<[ActionRequest]>([])
        let store = store(reply: {
            await gate.wait()
            return try Fixtures.response(#"{"protocol":1,"outcome":"applied","message":"Quitado"}"#)
        }, requests: requests)
        await store.send(.loaded(try Fixtures.document("machine-settings-ios-es")))
        let rule = try XCTUnwrap(store.state.rules.first)

        await store.send(.remove(rule))
        XCTAssertEqual(store.state.shown.map(\.id), ["rule-4"], "it goes before the daemon answers")
        await gate.open()
        await store.receive(\.answered)
        XCTAssertNil(store.state.error)
        XCTAssertEqual(requests.value.map(\.actionId), [rule.remove.id])
        XCTAssertEqual(store.state.shown.map(\.id), ["rule-4"], "kept hidden until the Mac's view drops it")
    }

    func testARefusedRemovalBringsTheRuleBackWithTheDaemonsReason() async throws {
        let requests = LockIsolated<[ActionRequest]>([])
        let store = store(reply: {
            try Fixtures.response(#"{"protocol":1,"outcome":"rejected","message":"Alondra no pudo cambiar el fichero de ajustes del repo"}"#)
        }, requests: requests)
        await store.send(.loaded(try Fixtures.document("machine-settings-ios-es")))
        let rule = try XCTUnwrap(store.state.rules.first)

        await store.send(.remove(rule))
        await store.receive(\.answered)

        XCTAssertEqual(store.state.shown.map(\.id), ["rule-3", "rule-4"])
        XCTAssertEqual(store.state.error, "Alondra no pudo cambiar el fichero de ajustes del repo")
    }

    func testAnUnreachableMacBringsTheRuleBack() async throws {
        struct Offline: Error {}
        let store = store(reply: { throw Offline() }, requests: LockIsolated([]))
        await store.send(.loaded(try Fixtures.document("machine-settings-ios-es")))
        let rule = try XCTUnwrap(store.state.rules.first)

        await store.send(.remove(rule))
        await store.receive(\.failed)

        XCTAssertEqual(store.state.shown.map(\.id), ["rule-3", "rule-4"])
        XCTAssertEqual(store.state.error, String(localized: "Your Mac didn’t remove the permission. Try again."))
    }
}
