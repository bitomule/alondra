import AlondraAccount
import AlondraNotifications
import AppKit
import Foundation
import Observation
import SwiftUI

/// Shows the local daemon's notices as system notifications while Alondra is not in front, for
/// as long as the app runs, with or without a window. Remembers the card the user opened from a
/// notification so the window selects it.
@MainActor
@Observable
final class MacLocalNotices {
    var openedCard: String?
    @ObservationIgnored var openWindow: OpenWindowAction?
    @ObservationIgnored private let center = SystemLocalNoticeCenter()
    @ObservationIgnored private let coordinator = LocalNoticeCoordinator(
        center: SystemLocalNoticeCenter(), ledger: .load(from: .standard), save: LocalNoticeLedger.saver(to: .standard))
    @ObservationIgnored private let safetyInterval: Duration = .seconds(30)

    func run(owner: MacWorkspaceOwner) async {
        for await services in Observations({ owner.route == .local ? owner.services : nil }) {
            guard let services else { continue }
            await follow(services)
            return
        }
    }

    func open(_ card: String) {
        NSApplication.shared.activate()
        if !NSApplication.shared.windows.contains(where: \.isVisible) { openWindow?(id: "main") }
        openedCard = card
    }

    private func follow(_ services: Connection) async {
        await center.requestAuthorization()
        let changes = await services.viewsFeed.events()
        let (ticks, tick) = AsyncStream<Void>.makeStream(bufferingPolicy: .bufferingNewest(1))
        await withTaskGroup { group in
            group.addTask {
                for await _ in changes { tick.yield() }
            }
            group.addTask { [safetyInterval] in
                while !Task.isCancelled {
                    tick.yield()
                    try? await Task.sleep(for: safetyInterval)
                }
            }
            for await _ in ticks {
                guard let update = try? await services.localNotices() else { continue }
                await coordinator.apply(update, appActive: NSApplication.shared.isActive)
            }
            group.cancelAll()
        }
    }
}
