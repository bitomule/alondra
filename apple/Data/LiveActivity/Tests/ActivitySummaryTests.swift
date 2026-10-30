import AlondraLiveActivity
import Foundation
import Testing

@Suite struct ActivitySummaryTests {
    private let now = Date(timeIntervalSince1970: 1_800_000_000)

    private func report(_ mac: String, _ counts: ActivityCounts, minutesAgo: Double) -> ActivityReport {
        ActivityReport(macID: mac, macName: mac, counts: counts, measuredAt: now - minutesAgo * 60)
    }

    @Test func contentStateSurvivesActivityKitsDefaultCoders() throws {
        let state = ActivityContentState(reports: [report("Mac mini", ActivityCounts(inbox: 3, waiting: 1, working: 2), minutesAgo: 1)])
        let decoded = try JSONDecoder().decode(ActivityContentState.self, from: JSONEncoder().encode(state))
        #expect(decoded == state)
    }

    @Test func sumsTheMacsAndSaysNothingWhileAllIsFresh() {
        let summary = ActivitySummary(ActivityContentState(reports: [
            report("a", ActivityCounts(inbox: 1, waiting: 0, working: 2), minutesAgo: 1),
            report("b", ActivityCounts(inbox: 2, waiting: 1, working: 0), minutesAgo: 29),
        ]), now: now, stale: false)
        #expect(summary.totals == ActivityCounts(inbox: 3, waiting: 1, working: 2))
        #expect(summary.notice == nil)
        #expect(summary.updatedAt == now - 60)
    }

    @Test func namesTheMacQuietForLongestAfterThirtyMinutes() {
        let summary = ActivitySummary(ActivityContentState(reports: [
            report("a", ActivityCounts(inbox: 1, waiting: 0, working: 0), minutesAgo: 31),
            report("b", ActivityCounts(inbox: 0, waiting: 0, working: 1), minutesAgo: 45),
            report("c", ActivityCounts(inbox: 0, waiting: 0, working: 1), minutesAgo: 2),
        ]), now: now, stale: false)
        #expect(summary.notice == .noNewsFromMac(name: "b", since: now - 45 * 60))
        #expect(summary.totals.working == 2)
    }

    @Test func aStaleActivitySaysNoMacHasNews() {
        let summary = ActivitySummary(ActivityContentState(reports: [
            report("a", ActivityCounts(inbox: 1, waiting: 0, working: 0), minutesAgo: 40),
        ]), now: now, stale: true)
        #expect(summary.notice == .noNewsFromAnyMac(since: now - 40 * 60))
    }

    @Test func allZeroIsAllCaughtUp() {
        let summary = ActivitySummary(ActivityContentState(reports: [report("a", .zero, minutesAgo: 3)]), now: now, stale: false)
        #expect(summary.notice == .allCaughtUp)
    }

    @Test func aReportFromAMacWhoseClockRunsFastCountsAsNow() {
        let summary = ActivitySummary(ActivityContentState(reports: [
            report("a", ActivityCounts(inbox: 1, waiting: 0, working: 0), minutesAgo: -20),
        ]), now: now, stale: false)
        #expect(summary.updatedAt == now)
        #expect(summary.notice == nil)
    }
}
