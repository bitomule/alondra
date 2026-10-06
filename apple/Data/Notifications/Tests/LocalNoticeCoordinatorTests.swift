import Foundation
import Synchronization
import Testing
@testable import AlondraNotifications

private final class FakeCenter: LocalNoticeCenter {
    let shown = Mutex<[LocalNotices.Notice]>([])
    let showing = Mutex<Set<String>>([])
    let badge = Mutex<Int?>(nil)

    func show(_ notice: LocalNotices.Notice) async {
        shown.withLock { $0.append(notice) }
        _ = showing.withLock { $0.insert(notice.card) }
    }
    func delivered() async -> [String] { showing.withLock { $0.sorted() } }
    func remove(_ cards: [String]) async { showing.withLock { $0.subtract(cards) } }
    func setBadge(_ count: Int) async { badge.withLock { $0 = count } }
}

private final class Saved: Sendable {
    let ledger = Mutex<LocalNoticeLedger?>(nil)
}

struct LocalNoticeCoordinatorTests {
    private let question = LocalNotices.Notice(id: 2, card: "t-1-ask-1", title: "Pick the parser",
                                               body: "The tests need one.", thread: "t-1", level: "active")
    private let result = LocalNotices.Notice(id: 5, card: "t-2-result-1", title: "Review PR #12", thread: "t-2")

    private func coordinator(_ center: FakeCenter, ledger: LocalNoticeLedger = LocalNoticeLedger(epoch: "e1"),
                             saved: Saved = Saved()) -> LocalNoticeCoordinator {
        LocalNoticeCoordinator(center: center, ledger: ledger, save: { ledger in saved.ledger.withLock { $0 = ledger } })
    }

    @Test func withTheAppInFrontNothingIsShownButTheNoticeIsRemembered() async {
        let center = FakeCenter()
        let coordinator = coordinator(center)
        await coordinator.apply(LocalNotices(dataEpoch: "e1", notices: [question], open: [question.card]), appActive: true)
        await coordinator.apply(LocalNotices(dataEpoch: "e1", notices: [question], open: [question.card]), appActive: false)
        #expect(center.shown.withLock { $0 }.isEmpty)
        #expect(center.badge.withLock { $0 } == 1)
    }

    @Test func inTheBackgroundEachNewNoticeIsShownOnce() async {
        let center = FakeCenter()
        let coordinator = coordinator(center)
        await coordinator.apply(LocalNotices(dataEpoch: "e1", notices: [question], open: [question.card]), appActive: false)
        await coordinator.apply(LocalNotices(dataEpoch: "e1", notices: [question, result],
                                             open: [question.card, result.card]), appActive: false)
        await coordinator.apply(LocalNotices(dataEpoch: "e1", notices: [question, result],
                                             open: [question.card, result.card]), appActive: false)
        #expect(center.shown.withLock { $0 } == [question, result])
        #expect(center.shown.withLock { $0 }.first?.thread == "t-1", "grouped by Task like the phone's push")
    }

    @Test func aReconnectWithTheSavedLedgerRepeatsNothing() async {
        let saved = Saved()
        let first = FakeCenter()
        await coordinator(first, saved: saved)
            .apply(LocalNotices(dataEpoch: "e1", notices: [question], open: [question.card]), appActive: false)
        let relaunched = FakeCenter()
        let ledger = try! #require(saved.ledger.withLock { $0 })
        await coordinator(relaunched, ledger: ledger, saved: saved)
            .apply(LocalNotices(dataEpoch: "e1", notices: [question, result],
                                open: [question.card, result.card]), appActive: false)
        #expect(first.shown.withLock { $0 } == [question])
        #expect(relaunched.shown.withLock { $0 } == [result])
    }

    @Test func aFirstLaunchOrANewDaemonDatabaseNeverAnnouncesTheBacklog() async {
        let center = FakeCenter()
        let fresh = coordinator(center, ledger: LocalNoticeLedger())
        await fresh.apply(LocalNotices(dataEpoch: "e1", notices: [question], open: [question.card]), appActive: false)
        let reset = coordinator(center, ledger: LocalNoticeLedger(epoch: "old", seen: [1]))
        await reset.apply(LocalNotices(dataEpoch: "e1", notices: [question], open: [question.card]), appActive: false)
        #expect(center.shown.withLock { $0 }.isEmpty)
        await reset.apply(LocalNotices(dataEpoch: "e1", notices: [question, result],
                                       open: [question.card, result.card]), appActive: false)
        #expect(center.shown.withLock { $0 } == [result])
    }

    @Test func aCardThatLeavesTheQueueTakesItsNotificationAndTheBadgeDown() async {
        let center = FakeCenter()
        let coordinator = coordinator(center)
        await coordinator.apply(LocalNotices(dataEpoch: "e1", notices: [question, result],
                                             open: [question.card, result.card]), appActive: false)
        #expect(await center.delivered() == [question.card, result.card])
        #expect(center.badge.withLock { $0 } == 2)
        await coordinator.apply(LocalNotices(dataEpoch: "e1", notices: [result], open: [result.card]), appActive: true)
        #expect(await center.delivered() == [result.card])
        #expect(center.badge.withLock { $0 } == 1)
    }

    @Test func theLedgerKeepsOnlyTheNewestNotices() {
        var ledger = LocalNoticeLedger(epoch: "e1")
        ledger.remember(1...Int64(LocalNoticeLedger.limit + 10))
        #expect(ledger.seen.count == LocalNoticeLedger.limit)
        #expect(ledger.seen.min() == 11)
    }

    @Test func theDecoderReadsWhatTheDaemonSends() throws {
        let env = ProcessInfo.processInfo.environment
        let root = URL(fileURLWithPath: env["TEST_SRCDIR"] ?? "").appendingPathComponent(env["TEST_WORKSPACE"] ?? "_main")
        let url = root.appendingPathComponent("fixtures/views/local-notices.json")
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let notices = try decoder.decode(LocalNotices.self, from: Data(contentsOf: url))
        #expect(notices == LocalNotices(dataEpoch: "fixture-epoch", notices: [
            .init(id: 2, card: "question-a", title: "Pick the parser", body: "The tests need one.",
                  thread: "task-a", level: "active")], open: ["question-a"]))
    }
}
