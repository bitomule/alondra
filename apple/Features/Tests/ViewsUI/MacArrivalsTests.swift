import AlondraContract
import AlondraFeed
import AlondraFeatures
import ComposableArchitecture
import Foundation
import XCTest

/// A card that arrives while the person reads another never takes its place: the open card stays,
/// the sidebar announces the new one, and it is the one that comes next.
@MainActor
final class MacArrivalsTests: XCTestCase {
    func testACardThatArrivesWhileReadingLeavesTheOpenCardAndIsAnnounced() async throws {
        let store = try await reading()

        XCTAssertEqual(store.state.selectedViewKey, "card:report", "the card being read stays open")
        XCTAssertEqual(store.state.detailDocument?.key, "card:report")
        XCTAssertEqual(store.state.arrivals, ["card:question"])
        await store.skipInFlightEffects(strict: false)
    }

    func testTheAnnouncementOpensTheNewCard() async throws {
        let store = try await reading()

        await store.send(.select(.viewKey("card:question")))
        await store.skipReceivedActions(strict: false)
        XCTAssertEqual(store.state.selectedViewKey, "card:question")
        XCTAssertEqual(store.state.arrivals, [])
        await store.skipInFlightEffects(strict: false)
    }

    func testTheNewCardComesNextWhenTheOpenOneLeaves() async throws {
        let store = try await reading()

        await store.send(.viewLoaded("sidebar", .document(try Self.sidebar(["question", "other"], version: "left"))))
        await store.skipReceivedActions(strict: false)
        XCTAssertEqual(store.state.selectedViewKey, "card:question", "not the card that followed the report in the list")
        XCTAssertEqual(store.state.arrivals, [])
        await store.skipInFlightEffects(strict: false)
    }

    func testANewCardTheDaemonWithdrawsIsNoLongerAnnounced() async throws {
        let store = try await reading()

        await store.send(.viewLoaded("sidebar", .document(try Self.sidebar(["report", "other"], version: "withdrawn"))))
        await store.skipReceivedActions(strict: false)
        XCTAssertEqual(store.state.selectedViewKey, "card:report")
        XCTAssertEqual(store.state.arrivals, [])
        await store.skipInFlightEffects(strict: false)
    }

    /// The report is open, then a question that outranks it arrives first in the list.
    private func reading() async throws -> TestStoreOf<MacPresentationFeature> {
        let store = try Self.store(sidebar: Self.sidebar(["report", "other"], version: "reading"))
        await store.send(.appeared)
        await store.skipReceivedActions(strict: false)
        XCTAssertEqual(store.state.selectedViewKey, "card:report")
        XCTAssertEqual(store.state.arrivals, [], "the first sidebar announces nothing")

        await store.send(.viewLoaded("sidebar", .document(try Self.sidebar(["question", "report", "other"], version: "arrived"))))
        await store.skipReceivedActions(strict: false)
        return store
    }

    private static func store(sidebar: ViewDocument) throws -> TestStoreOf<MacPresentationFeature> {
        let store = TestStore(initialState: MacPresentationFeature.State()) { MacPresentationFeature() } withDependencies: {
            $0.continuousClock = TestClock()
            $0.viewsClient.openScreen = { _, _ in }
            $0.viewsClient.closeScreen = { _ in }
            $0.viewsClient.load = { key, _ in
                key.rawValue == "sidebar" ? .document(sidebar) : .document(try Self.document(key: key.rawValue))
            }
        }
        store.exhaustivity = .off
        return store
    }

    nonisolated private static func sidebar(_ waiting: [String], version: String) throws -> ViewDocument {
        let rows = waiting.map {
            #"{"type":"row","id":"\#($0)","icon":"question","title":"Card \#($0)","depth":0,"struck":false,"link":{"kind":"view","key":"card:\#($0)"},"children":[]}"#
        }.joined(separator: ",")
        return try ContractCodec.decode(ViewDocument.self, from: Data(json(key: "sidebar", version: version,
            sections: #"[{"id":"waiting","collapsed":false,"items":[\#(rows)]}]"#).utf8))
    }

    nonisolated private static func document(key: String) throws -> ViewDocument {
        try ContractCodec.decode(ViewDocument.self, from: Data(json(key: key, version: "v-\(key)", sections: "[]").utf8))
    }

    nonisolated private static func json(key: String, version: String, sections: String) -> String {
        let status = #"{"version":"s1","summary":"Ready","badge":0,"counts":[],"gauges":[],"orchestrator":{"text":"Ready","tone":"success"}}"#
        return #"{"protocol":1,"view":"\#(key)","key":"\#(key)","client":"mac","version":"\#(version)","cursor":"c1.abc","data_epoch":"epoch","status":\#(status),"sections":\#(sections),"notices":[],"first_cards":[]}"#
    }
}
