import AlondraLiveActivity
import AlondraTransport
import Foundation
import Testing

@MainActor
private final class FakeHost: ActivityHost {
    enum Call: Hashable {
        case request(ActivityContentState)
        case update(String, ActivityContentState)
        case end(String, ActivityDismissal)
    }

    var activitiesEnabled = true
    var activities: [RunningActivity] = []
    var calls: [Call] = []
    var contents: [String: ActivityContentState] = [:]
    private var next = 0

    func running() -> [RunningActivity] { activities }

    func content(of id: String) -> ActivityContentState? { contents[id] }

    func request(_ state: ActivityContentState, startedAt: Date, staleDate: Date) throws -> RunningActivity {
        next += 1
        let activity = RunningActivity(id: "activity-\(next)", startedAt: startedAt)
        activities.append(activity)
        calls.append(.request(state))
        return activity
    }

    func update(_ id: String, to state: ActivityContentState, staleDate: Date) async {
        calls.append(.update(id, state))
    }

    func end(_ id: String, with state: ActivityContentState?, dismissal: ActivityDismissal) async {
        activities.removeAll { $0.id == id }
        calls.append(.end(id, dismissal))
    }

    var requests: Int { calls.filter { if case .request = $0 { true } else { false } }.count }
    var ends: [Call] { calls.filter { if case .end = $0 { true } else { false } } }
}

@MainActor
private final class Harness {
    let host = FakeHost()
    var now = Date(timeIntervalSince1970: 1_800_000_000)
    var wakes: [Date] = []
    var opened: [ActivityReport] = []
    lazy var controller = LiveActivityController(host: host, now: { [unowned self] in now }, open: { [unowned self] _ in opened },
                                                 wake: { [unowned self] in wakes.append($0) })

    func report(_ inbox: Int, _ waiting: Int = 0, _ working: Int = 0, mac: String = "mac-a", live: Bool = true) async {
        controller.report(macID: mac, name: "Mac mini", counts: ActivityCounts(inbox: inbox, waiting: waiting, working: working), live: live)
        await controller.settled()
    }

    func foreground() async {
        controller.enteredForeground()
        await controller.settled()
    }

    func background() async {
        controller.enteredBackground()
        await controller.settled()
    }
}

@MainActor
@Suite struct LiveActivityControllerTests {
    @Test func startsInTheForegroundOnlyWhenSomethingIsPending() async {
        let harness = Harness()
        await harness.foreground()
        await harness.report(0)
        #expect(harness.host.requests == 0)
        await harness.report(2, 1, 3)
        #expect(harness.host.requests == 1)
        #expect(harness.host.calls.first == .request(ActivityContentState(reports: [
            ActivityReport(macID: "mac-a", macName: "Mac mini", counts: ActivityCounts(inbox: 2, waiting: 1, working: 3), measuredAt: harness.now),
        ])))
    }

    private func envelope(from mac: String) -> Envelope {
        Envelope(route: mac, sender: mac, recipient: "phone", requestID: "request", kind: "activity", expiresAtMilliseconds: 1,
                 ephemeralKey: "key", nonce: "nonce", ciphertext: "ciphertext", signature: "signature")
    }

    private func runningWithPush(_ harness: Harness, macs: [String]) {
        harness.host.activities = [RunningActivity(id: "pushed", startedAt: harness.now)]
        harness.host.contents["pushed"] = ActivityContentState(macs: macs.map(envelope(from:)), omitted: 2)
    }

    @Test func anUpdateFromTheAppKeepsTheOtherMacsOfThePush() async throws {
        let harness = Harness()
        runningWithPush(harness, macs: ["mac-b"])
        harness.opened = [ActivityReport(macID: "mac-b", macName: nil, counts: ActivityCounts(inbox: 1, waiting: 0, working: 0), measuredAt: harness.now)]
        await harness.foreground()
        await harness.report(2)
        guard case .update(_, let state) = try #require(harness.host.calls.last) else { Issue.record("no update"); return }
        #expect(state.macs == [envelope(from: "mac-b")])
        #expect(state.omitted == 2)
        #expect(state.reports.map(\.macID) == ["mac-a"])
    }

    @Test func aMacTheAppKnowsBetterLeavesThePush() async throws {
        let harness = Harness()
        runningWithPush(harness, macs: ["mac-a"])
        harness.opened = [ActivityReport(macID: "mac-a", macName: nil, counts: ActivityCounts(inbox: 9, waiting: 0, working: 0),
                                         measuredAt: harness.now.addingTimeInterval(-60))]
        await harness.foreground()
        await harness.report(2)
        guard case .update(_, let state) = try #require(harness.host.calls.last) else { Issue.record("no update"); return }
        #expect(state.macs.isEmpty)
    }

    @Test func anotherMacStillBusyKeepsItShowingWhenTheAppsMacIsDone() async {
        let harness = Harness()
        runningWithPush(harness, macs: ["mac-b"])
        harness.opened = [ActivityReport(macID: "mac-b", macName: nil, counts: ActivityCounts(inbox: 0, waiting: 0, working: 1), measuredAt: harness.now)]
        await harness.foreground()
        await harness.report(0)
        #expect(harness.host.ends.isEmpty)
    }

    @Test func numbersSavedFromBeforeNeverEndWhatASealedReportSaysIsBusy() async {
        let harness = Harness()
        runningWithPush(harness, macs: ["mac-a"])
        harness.opened = [ActivityReport(macID: "mac-a", macName: nil, counts: ActivityCounts(inbox: 0, waiting: 0, working: 2),
                                         measuredAt: harness.now.addingTimeInterval(-60))]
        await harness.foreground()
        await harness.report(0, live: false)
        #expect(harness.host.ends.isEmpty)
    }

    @Test func neverStartsWithLiveActivitiesTurnedOff() async {
        let harness = Harness()
        harness.host.activitiesEnabled = false
        await harness.foreground()
        await harness.report(2)
        #expect(harness.host.calls.isEmpty)
    }

    @Test func waitsForTheForegroundToStartOne() async {
        let harness = Harness()
        await harness.background()
        await harness.report(1)
        #expect(harness.host.requests == 0)
        await harness.foreground()
        #expect(harness.host.requests == 1)
    }

    @Test func updatesTheRunningOneInTheBackground() async {
        let harness = Harness()
        await harness.foreground()
        await harness.report(1)
        await harness.background()
        harness.now += 60
        await harness.report(4, live: false)
        #expect(harness.host.requests == 1)
        guard case .update("activity-1", let state) = harness.host.calls.last else {
            Issue.record("expected an update, got \(harness.host.calls)")
            return
        }
        #expect(state.reports.map(\.counts.inbox) == [4])
    }

    @Test func sumsEveryMacItKnows() async {
        let harness = Harness()
        await harness.foreground()
        await harness.report(1, 0, 2, mac: "mac-a")
        await harness.report(3, 1, 0, mac: "mac-b")
        guard case .update(_, let state) = harness.host.calls.last else {
            Issue.record("expected an update, got \(harness.host.calls)")
            return
        }
        #expect(state.reports.map(\.macID) == ["mac-a", "mac-b"])
        #expect(ActivitySummary(state, now: harness.now, stale: false).totals == ActivityCounts(inbox: 4, waiting: 1, working: 2))
    }

    @Test func staysFifteenMinutesAfterEverythingReachesZeroThenEnds() async {
        let harness = Harness()
        await harness.foreground()
        await harness.report(1)
        let zeroAt = harness.now
        await harness.report(0)
        #expect(harness.host.ends.isEmpty)
        #expect(harness.wakes.last == zeroAt + LiveActivityController.grace)
        harness.now = zeroAt + LiveActivityController.grace - 1
        harness.controller.reconcile()
        await harness.controller.settled()
        #expect(harness.host.ends.isEmpty)
        harness.now = zeroAt + LiveActivityController.grace
        harness.controller.reconcile()
        await harness.controller.settled()
        #expect(harness.host.ends == [.end("activity-1", .immediate)])
    }

    @Test func workThatComesBackWithinTheGraceKeepsTheSameOne() async {
        let harness = Harness()
        await harness.foreground()
        await harness.report(1)
        await harness.report(0)
        harness.now += 60
        await harness.report(0, 0, 1)
        #expect(harness.host.requests == 1)
        #expect(harness.host.ends.isEmpty)
    }

    @Test func goingToTheBackgroundDuringTheGraceEndsItWhenTheGraceRunsOut() async {
        let harness = Harness()
        await harness.foreground()
        await harness.report(1)
        let zeroAt = harness.now
        await harness.report(0)
        await harness.background()
        #expect(harness.host.ends == [.end("activity-1", .after(zeroAt + LiveActivityController.grace))])
    }

    @Test func endsEveryDuplicateButTheNewest() async {
        let harness = Harness()
        harness.host.activities = [
            RunningActivity(id: "old", startedAt: harness.now - 600),
            RunningActivity(id: "newest", startedAt: harness.now - 10),
            RunningActivity(id: "older", startedAt: harness.now - 900),
        ]
        await harness.foreground()
        await harness.report(1)
        #expect(Set(harness.host.ends) == [.end("old", .immediate), .end("older", .immediate)])
        #expect(harness.host.activities.map(\.id) == ["newest"])
        #expect(harness.host.requests == 0)
    }

    @Test func forgettingEveryMacEndsIt() async {
        let harness = Harness()
        await harness.foreground()
        await harness.report(2)
        harness.controller.keep(macIDs: [])
        await harness.controller.settled()
        #expect(harness.host.ends == [.end("activity-1", .immediate)])
    }

    @Test func anActivityLeftFromBeforeWithNothingPendingEnds() async {
        let harness = Harness()
        harness.host.activities = [RunningActivity(id: "left", startedAt: harness.now - 3600)]
        await harness.foreground()
        await harness.report(0)
        #expect(harness.host.ends == [.end("left", .immediate)])
    }

    @Test func aLaunchLeavesARunningActivityAloneUntilItKnowsTheMacs() async {
        let harness = Harness()
        harness.host.activities = [RunningActivity(id: "pushed", startedAt: harness.now - 60)]
        await harness.foreground()
        #expect(harness.host.calls.isEmpty)
        await harness.report(3)
        #expect(harness.host.calls.count == 1)
        guard case .update("pushed", _) = harness.host.calls.first else {
            Issue.record("expected an update of the running one, got \(harness.host.calls)")
            return
        }
    }

    @Test func aMacThatStopsAnsweringKeepsTheTimeItWasLastHeard() async {
        let harness = Harness()
        await harness.foreground()
        await harness.report(1)
        let heard = harness.now
        await harness.report(1, live: false)
        harness.now += 3600
        await harness.report(2, live: false)
        guard case .update(_, let state) = harness.host.calls.last else {
            Issue.record("expected an update, got \(harness.host.calls)")
            return
        }
        #expect(state.reports.first?.measuredAt == heard)
    }
}
