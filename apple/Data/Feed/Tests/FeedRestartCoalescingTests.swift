import AlondraContract
@testable import AlondraFeed
import AlondraTransport
import Foundation
import XCTest

final class FeedRestartCoalescingTests: XCTestCase {
    /// 7 Nov, iPhone on mobile data: 83 of 133 long polls were cancelled, 66 of them within a second
    /// of leaving, because each view that opened restarted the poll to add its key.
    func testViewsOpeningOneAfterAnotherSendOnePollForAll() async throws {
        let transport = CountingMac()
        let session = FeedSession(transport: transport, client: .ios, lang: "en", restartCoalescing: .milliseconds(200))
        let events = await session.events()
        _ = try await session.open(ViewKey.home.rawValue)
        _ = try await session.open(ViewKey.schedules.rawValue)
        _ = try await session.open(ViewKey.tasks.rawValue)
        try await transport.waitForPoll()
        try await Task.sleep(for: .milliseconds(400))
        let polls = await transport.polls
        XCTAssertEqual(polls.count, 1, "three opens inside the window start one poll")
        XCTAssertEqual(Set(polls[0]), ["home", "schedules", "tasks"])
        await session.stop()
        withExtendedLifetime(events) {}
    }
}

private actor CountingMac: ViewTransport {
    private(set) var polls: [[String]] = []
    private var waiter: CheckedContinuation<Void, Never>?

    func waitForPoll() async throws {
        if !polls.isEmpty { return }
        await withCheckedContinuation { waiter = $0 }
    }

    func fetchView(key: String, client: ViewClient, lang: String, known: String?) async throws -> ViewFetch {
        let json = """
        {"protocol":1,"view":"\(key)","key":"\(key)","client":"ios","version":"v","cursor":"c","data_epoch":"e",
        "status":{"version":"s","summary":"Ready","badge":0,"counts":[],"gauges":[],
        "orchestrator":{"text":"idle","tone":"neutral"}},"sections":[]}
        """
        return try ContractCodec.decode(ViewFetch.self, from: Data(json.utf8))
    }

    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed {
        polls.append(views.map(\.key))
        waiter?.resume()
        waiter = nil
        try await Task.sleep(for: .seconds(60))
        throw CancellationError()
    }

    func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse {
        throw FeedError.unknownCommand
    }
}
