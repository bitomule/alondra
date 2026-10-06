import Foundation

/// Turns the daemon's notices into system notifications while the app is not in front. With the
/// app in front the user sees the card in the window, so it is remembered without a notification.
public actor LocalNoticeCoordinator {
    private let center: any LocalNoticeCenter
    private let save: @Sendable (LocalNoticeLedger) -> Void
    private var ledger: LocalNoticeLedger

    public init(center: any LocalNoticeCenter, ledger: LocalNoticeLedger,
                save: @escaping @Sendable (LocalNoticeLedger) -> Void) {
        self.center = center
        self.ledger = ledger
        self.save = save
    }

    public func apply(_ update: LocalNotices, appActive: Bool) async {
        let fresh = update.notices.filter { !ledger.seen.contains($0.id) }
        if ledger.epoch != update.dataEpoch {
            ledger = LocalNoticeLedger(epoch: update.dataEpoch)
        } else if !appActive {
            for notice in fresh { await center.show(notice) }
        }
        let waiting = Set(update.open)
        let stale = await center.delivered().filter { !waiting.contains($0) }
        if !stale.isEmpty { await center.remove(stale) }
        await center.setBadge(update.open.count)
        let before = ledger
        ledger.remember(update.notices.map(\.id))
        if ledger != before { save(ledger) }
    }
}
