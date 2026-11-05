import Foundation

/// One Live Activity of Alondra as iOS knows it.
public struct RunningActivity: Equatable, Sendable {
    public let id: String
    public let startedAt: Date

    public init(id: String, startedAt: Date) { self.id = id; self.startedAt = startedAt }
}

public enum ActivityDismissal: Hashable, Sendable {
    case immediate
    case after(Date)
}

/// What the controller needs from ActivityKit, so its rules run against a fake in tests.
@MainActor
public protocol ActivityHost: AnyObject {
    var activitiesEnabled: Bool { get }
    func running() -> [RunningActivity]
    /// What the activity shows now, whoever wrote it last: a push may have changed it.
    func content(of id: String) -> ActivityContentState?
    func request(_ state: ActivityContentState, startedAt: Date, staleDate: Date) throws -> RunningActivity
    func update(_ id: String, to state: ActivityContentState, staleDate: Date) async
    func end(_ id: String, with state: ActivityContentState?, dismissal: ActivityDismissal) async
}

/// Starts, updates and ends Alondra's one Live Activity from what the app knows of each Mac.
///
/// A Mac is active while it has something in Waiting for You, waiting or working, and for
/// `grace` after all three reach zero, so the Live Activity does not leave and come back between
/// one task and the next. It shows while any Mac is active. iOS only lets the app start one in
/// the foreground; it can update and end it from the background too.
///
/// The app rarely knows every Mac, so it writes over what the relay pushed only for the Macs it
/// has newer word of: the other Macs' sealed reports stay, and they keep it showing.
@MainActor
public final class LiveActivityController {
    public static let grace: TimeInterval = 15 * 60
    /// After this long without an update the Live Activity says it has no news.
    public static let staleAfter: TimeInterval = 30 * 60

    private struct Mac {
        var name: String?
        var counts: ActivityCounts
        var live: Bool
        var heardAt: Date
        var zeroSince: Date?
    }

    private let host: any ActivityHost
    private let now: () -> Date
    private let wake: (Date) -> Void
    private let open: (ActivityContentState) -> [ActivityReport]
    private var macs: [String: Mac] = [:]
    /// Until a Mac reports or the paired Macs are known, an activity already running is left alone:
    /// the app has just launched and has not loaded anything yet.
    private var knowsMacs = false
    private var foreground = false
    private var published: [String: ActivityContentState] = [:]
    private var tail: Task<Void, Never>?

    /// `wake` asks to be reconciled again at that date, when a Mac's grace runs out; `open` reads
    /// the sealed reports a push left in the activity.
    public init(host: any ActivityHost, now: @escaping () -> Date = Date.init,
                open: @escaping (ActivityContentState) -> [ActivityReport] = { _ in [] }, wake: @escaping (Date) -> Void) {
        self.host = host; self.now = now; self.open = open; self.wake = wake
    }

    /// The counts of one Mac as the app last loaded them; `live` while its connection answers.
    public func report(macID: String, name: String?, counts: ActivityCounts, live: Bool) {
        let at = now()
        var mac = macs[macID] ?? Mac(name: name, counts: counts, live: live, heardAt: at, zeroSince: nil)
        if mac.live || live { mac.heardAt = at }
        mac.name = name
        mac.live = live
        if !counts.isZero {
            mac.zeroSince = nil
        } else if !mac.counts.isZero {
            mac.zeroSince = at
        }
        mac.counts = counts
        macs[macID] = mac
        knowsMacs = true
        reconcile()
    }

    /// Forgets every Mac no longer paired; with none left the Live Activity ends.
    public func keep(macIDs: Set<String>) {
        macs = macs.filter { macIDs.contains($0.key) }
        knowsMacs = true
        reconcile()
    }

    public func enteredForeground() {
        foreground = true
        reconcile()
    }

    public func enteredBackground() {
        foreground = false
        reconcile()
    }

    public func reconcile() {
        let previous = tail
        tail = Task { [weak self] in
            await previous?.value
            await self?.apply()
        }
    }

    /// Waits until every change made so far has reached ActivityKit.
    public func settled() async { await tail?.value }

    private func apply() async {
        let at = now()
        for id in macs.keys where macs[id]?.live == true { macs[id]?.heardAt = at }
        let running = host.running().sorted { $0.startedAt > $1.startedAt }
        for duplicate in running.dropFirst() {
            await host.end(duplicate.id, with: nil, dismissal: .immediate)
            published[duplicate.id] = nil
        }
        let pushed = running.first.flatMap { host.content(of: $0.id) } ?? ActivityContentState()
        let opened = open(pushed)
        let sealed = Set(opened.map(\.macID))
        // Numbers saved from before a Mac that does not answer are no match for a sealed report.
        let known = macs.filter { $0.value.live || !sealed.contains($0.key) }
        let reports = known.keys.sorted().compactMap { id in
            known[id].map { ActivityReport(macID: id, macName: $0.name, counts: $0.counts, measuredAt: $0.heardAt) }
        }
        let outdated = Set(opened.filter { report in known[report.macID].map { report.measuredAt <= $0.heardAt } ?? false }.map(\.macID))
        let state = ActivityContentState(reports: reports, macs: pushed.macs.filter { !outdated.contains($0.sender) },
                                         omitted: pushed.omitted).fitted()
        let busy = known.values.contains { !$0.counts.isZero }
            || opened.contains { !outdated.contains($0.macID) && !$0.counts.isZero }
        let graceEnds = macs.values.compactMap { $0.zeroSince?.addingTimeInterval(Self.grace) }.filter { $0 > at }.max()
        guard busy || graceEnds != nil else {
            if knowsMacs, let current = running.first {
                await host.end(current.id, with: state, dismissal: .immediate)
                published[current.id] = nil
            }
            return
        }
        let staleDate = at.addingTimeInterval(Self.staleAfter)
        var current = running.first
        if current == nil, busy, foreground, host.activitiesEnabled,
           let started = try? host.request(state, startedAt: at, staleDate: staleDate) {
            published[started.id] = state
            current = started
        }
        guard let current else { return }
        if published[current.id] != state {
            await host.update(current.id, to: state, staleDate: staleDate)
            published[current.id] = state
        }
        guard !busy, let graceEnds else { return }
        if foreground {
            wake(graceEnds)
        } else {
            await host.end(current.id, with: state, dismissal: .after(graceEnds))
            published[current.id] = nil
        }
    }
}
