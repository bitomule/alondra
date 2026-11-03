import AlondraContract
@testable import AlondraFeed
import Foundation
import XCTest

/// The queue arrives with its first cards inside: the cards open from that answer, without asking
/// the Mac for each one, and a daemon that does not send them is asked as before.
final class FirstCardsTests: XCTestCase {
    private let queue = ["card:front", "card:second", "card:third"]

    func testTheFirstCardsOpenFromTheQueueWithoutAskingTheMac() async throws {
        let peer = QueuePeer(embeds: true)
        let session = FeedSession(transport: peer, client: .ios, lang: "es")
        let home = try await session.open("home")
        let opened = try await Self.openAll(queue, in: session)
        let cardCalls = await peer.cardCalls
        await session.stop()
        XCTAssertEqual(home.document?.firstCards.map(\.key), queue)
        XCTAssertEqual(opened.map(\.document?.key), queue)
        XCTAssertEqual(opened.map(\.document), home.document?.firstCards.map(Optional.some))
        XCTAssertEqual(cardCalls, [], "every card came inside the queue")
    }

    func testADaemonThatDoesNotSendThemIsAskedForEachCard() async throws {
        let peer = QueuePeer(embeds: false)
        let session = FeedSession(transport: peer, client: .ios, lang: "es")
        let home = try await session.open("home")
        let opened = try await Self.openAll(queue, in: session)
        let cardCalls = await peer.cardCalls
        await session.stop()
        XCTAssertEqual(home.document?.firstCards, [])
        XCTAssertEqual(opened.map(\.document?.key), queue)
        XCTAssertEqual(Set(cardCalls), Set(queue))
    }

    func testCardsOfAnotherDataEpochAreAskedFor() async throws {
        let peer = QueuePeer(embeds: true, cardEpoch: "older-epoch")
        let session = FeedSession(transport: peer, client: .ios, lang: "es")
        _ = try await session.open("home")
        let opened = try await Self.openAll(queue, in: session)
        let cardCalls = await peer.cardCalls
        await session.stop()
        XCTAssertEqual(opened.map(\.document?.dataEpoch), Array(repeating: "epoch", count: 3))
        XCTAssertEqual(Set(cardCalls), Set(queue))
    }

    func testACardThatLeavesTheQueueLeavesTheCacheUnlessAScreenHoldsIt() async throws {
        let peer = QueuePeer(embeds: true)
        let session = FeedSession(transport: peer, client: .ios, lang: "es")
        _ = try await session.open("home")
        let held = try await session.open("card:front")
        await peer.setQueue(["card:fourth"])
        _ = try await session.load("home")
        let snapshot = await session.currentSnapshot()
        await session.close(held.lease)
        let closed = await session.currentSnapshot()
        await session.stop()
        XCTAssertNotNil(snapshot.documents["card:fourth"])
        XCTAssertNil(snapshot.documents["card:second"])
        XCTAssertNil(snapshot.documents["card:third"])
        XCTAssertNotNil(snapshot.documents["card:front"], "an open screen keeps its card")
        XCTAssertNil(closed.documents["card:front"])
    }

    func testClosingTheQueueForgetsTheCardsNoScreenOpened() async throws {
        let peer = QueuePeer(embeds: true)
        let session = FeedSession(transport: peer, client: .ios, lang: "es")
        let home = try await session.open("home")
        await session.close(home.lease)
        let snapshot = await session.currentSnapshot()
        await session.stop()
        XCTAssertTrue(snapshot.documents.isEmpty)
    }

    /// One trip to the Mac costs what the iPhone measured over the relay on cellular, 63–94 ms:
    /// the time until the front card is ready, before and after.
    func testTheFrontCardIsReadyAfterOneTripInsteadOfTwo() async throws {
        let trip = Duration.milliseconds(80)
        var times: [Bool: Duration] = [:]
        for embeds in [false, true] {
            let peer = QueuePeer(embeds: embeds, trip: trip)
            let session = FeedSession(transport: peer, client: .ios, lang: "es")
            let clock = ContinuousClock()
            let start = clock.now
            let home = try await session.open("home")
            let ids = home.document?.sections.first?.cardIds ?? []
            let front = try await session.open("card:\(try XCTUnwrap(ids.first))")
            times[embeds] = clock.now - start
            XCTAssertEqual(front.document?.key, "card:front")
            await session.stop()
        }
        let before = try XCTUnwrap(times[false]), after = try XCTUnwrap(times[true])
        print("time_to_front_card trip_ms=80 before_ms=\(before.milliseconds) after_ms=\(after.milliseconds)")
        XCTAssertGreaterThanOrEqual(before, trip * 2)
        XCTAssertLessThan(after, trip * 2)
    }

    private static func openAll(_ keys: [String], in session: FeedSession) async throws -> [(lease: ViewLease, document: ViewDocument?)] {
        try await withThrowingTaskGroup(of: (Int, (lease: ViewLease, document: ViewDocument?)).self) { group in
            for (index, key) in keys.enumerated() { group.addTask { (index, try await session.open(key)) } }
            var opened: [(Int, (lease: ViewLease, document: ViewDocument?))] = []
            for try await result in group { opened.append(result) }
            return opened.sorted { $0.0 < $1.0 }.map(\.1)
        }
    }
}

private extension Duration {
    var milliseconds: Int { Int(components.seconds * 1000 + components.attoseconds / 1_000_000_000_000_000) }
}

private actor QueuePeer: ViewTransport {
    private let embeds: Bool
    private let cardEpoch: String
    private let trip: Duration
    private var queue = ["card:front", "card:second", "card:third"]
    private(set) var cardCalls: [String] = []

    init(embeds: Bool, cardEpoch: String = "epoch", trip: Duration = .zero) {
        self.embeds = embeds
        self.cardEpoch = cardEpoch
        self.trip = trip
    }

    func setQueue(_ queue: [String]) { self.queue = queue }

    func fetchView(key: String, client: ViewClient, lang: String, known: String?) async throws -> ViewFetch {
        if trip > .zero { try await Task.sleep(for: trip) }
        var body: [String: Any]
        if key == "home" {
            body = Self.view(key: "home", epoch: "epoch", sections: [["id": "queue", "collapsed": false, "items": [],
                "card_ids": queue.map { String($0.dropFirst("card:".count)) }]])
            if embeds { body["first_cards"] = queue.prefix(3).map { Self.view(key: $0, epoch: cardEpoch, sections: []) } }
        } else {
            cardCalls.append(key)
            body = Self.view(key: key, epoch: "epoch", sections: [])
        }
        return try ContractCodec.decode(ViewFetch.self, from: try JSONSerialization.data(withJSONObject: body, options: [.sortedKeys]))
    }

    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed {
        try await Task.sleep(for: .seconds(3600))
        throw CancellationError()
    }

    func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse { throw CancellationError() }

    private static func view(key: String, epoch: String, sections: [[String: Any]]) -> [String: Any] {
        ["protocol": 1, "view": key.hasPrefix("card:") ? "card" : key, "key": key, "client": "ios", "version": "v-\(key)",
         "cursor": "c1", "data_epoch": epoch, "sections": sections,
         "status": ["version": "s", "summary": "", "badge": 0, "counts": [], "gauges": [],
                    "orchestrator": ["text": "idle", "tone": "neutral"]]]
    }
}
