import AlondraContract
import AlondraFeed
import AlondraFeatures
import ComposableArchitecture
import Foundation
import XCTest

/// The Mac's sidebar arrives with the first cards inside: the one it selects, or the one the person
/// clicks, is drawn from that answer; a daemon that does not send them is asked as before.
@MainActor
final class MacFirstCardTests: XCTestCase {
    func testTheFirstCardIsDrawnFromTheSidebarWithoutAskingTheMac() async throws {
        let (store, loads) = try Self.store(sidebar: Self.sidebar(waiting: ["a", "b", "c", "d"], carrying: ["a", "b", "c"]))
        await store.send(.appeared)
        await store.skipReceivedActions(strict: false)
        XCTAssertEqual(store.state.selectedViewKey, "card:a")
        XCTAssertEqual(store.state.detailDocument, store.state.sidebarDocument?.firstCards.first)
        XCTAssertEqual(loads.value, ["sidebar"])

        await store.send(.select(.viewKey("card:c")))
        await store.skipReceivedActions(strict: false)
        XCTAssertEqual(store.state.detailDocument?.key, "card:c")
        XCTAssertEqual(loads.value, ["sidebar"], "a card the sidebar carries is drawn without asking")

        await store.send(.select(.viewKey("card:d")))
        await store.skipReceivedActions(strict: false)
        XCTAssertEqual(store.state.detailDocument?.key, "card:d")
        XCTAssertEqual(loads.value, ["sidebar", "card:d"], "a card past the ones carried is asked for")
        await store.skipInFlightEffects(strict: false)
    }

    func testASidebarWithoutCardsAsksForTheFirstOne() async throws {
        let (store, loads) = try Self.store(sidebar: Self.sidebar(waiting: ["a", "b"], carrying: []))
        await store.send(.appeared)
        await store.skipReceivedActions(strict: false)
        XCTAssertEqual(store.state.selectedViewKey, "card:a")
        XCTAssertEqual(store.state.detailDocument?.key, "card:a")
        XCTAssertEqual(loads.value, ["sidebar", "card:a"])
        await store.skipInFlightEffects(strict: false)
    }

    private static func store(sidebar: ViewDocument) throws -> (TestStoreOf<MacPresentationFeature>, LockIsolated<[String]>) {
        let loads = LockIsolated<[String]>([])
        let store = TestStore(initialState: MacPresentationFeature.State()) { MacPresentationFeature() } withDependencies: {
            $0.continuousClock = TestClock()
            $0.viewsClient.openScreen = { _, _ in }
            $0.viewsClient.closeScreen = { _ in }
            $0.viewsClient.load = { key, _ in
                loads.withValue { $0.append(key.rawValue) }
                return key.rawValue == "sidebar" ? .document(sidebar) : .document(try Self.document(key: key.rawValue))
            }
        }
        store.exhaustivity = .off
        return (store, loads)
    }

    private static func sidebar(waiting: [String], carrying: [String]) throws -> ViewDocument {
        let rows = waiting.map {
            #"{"type":"row","id":"\#($0)","icon":"question","title":"Card \#($0)","depth":0,"struck":false,"link":{"kind":"view","key":"card:\#($0)"},"children":[]}"#
        }.joined(separator: ",")
        let cards = carrying.map { json(key: "card:\($0)", sections: "[]") }.joined(separator: ",")
        return try ContractCodec.decode(ViewDocument.self, from: Data(json(key: "sidebar",
            sections: #"[{"id":"waiting","collapsed":false,"items":[\#(rows)]}]"#, firstCards: "[\(cards)]").utf8))
    }

    nonisolated private static func document(key: String) throws -> ViewDocument {
        try ContractCodec.decode(ViewDocument.self, from: Data(json(key: key, sections: "[]").utf8))
    }

    nonisolated private static func json(key: String, sections: String, firstCards: String = "[]") -> String {
        let status = #"{"version":"s1","summary":"Ready","badge":0,"counts":[],"gauges":[],"orchestrator":{"text":"Ready","tone":"success"}}"#
        return #"{"protocol":1,"view":"\#(key)","key":"\#(key)","client":"mac","version":"v-\#(key)","cursor":"c1.abc","data_epoch":"epoch","status":\#(status),"sections":\#(sections),"notices":[],"first_cards":\#(firstCards)}"#
    }
}
