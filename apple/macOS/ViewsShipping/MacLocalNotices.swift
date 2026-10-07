import AlondraAccount
import AlondraFeatures
import AlondraNotifications
import AppKit
import Foundation
import Observation
import SwiftUI
import UserNotifications

/// Shows the local daemon's notices as system notifications while Alondra is not in front, for
/// as long as the app runs, with or without a window. Remembers the card the user opened from a
/// notification so the window selects it. Also asks, on this Mac, whether to allow each device
/// that wants to pair: a notification with Allow and Deny, and the same request in the window.
@MainActor
@Observable
final class MacLocalNotices {
    nonisolated static let pairingCategory = "alondra.pairing"
    nonisolated static let allowAction = "alondra.pairing.allow"
    nonisolated static let denyAction = "alondra.pairing.deny"

    var openedCard: String?
    var pairingRequests: [PairingRequest] = []
    var dismissedPairings: Set<String> = []
    var pairingError: String?
    @ObservationIgnored var openWindow: OpenWindowAction?
    @ObservationIgnored private let center = SystemLocalNoticeCenter()
    @ObservationIgnored private let coordinator = LocalNoticeCoordinator(
        center: SystemLocalNoticeCenter(), ledger: .load(from: .standard), save: LocalNoticeLedger.saver(to: .standard))
    @ObservationIgnored private let safetyInterval: Duration = .seconds(30)
    @ObservationIgnored private let pairingInterval: Duration = .seconds(3)
    @ObservationIgnored private var thisMac: Connection?
    @ObservationIgnored private var notifiedPairings: Set<String> = []

    /// The request the window asks about, unless the person put it aside.
    var shownPairing: PairingRequest? { pairingRequests.first { !dismissedPairings.contains($0.id) } }

    func run(owner: MacWorkspaceOwner) async {
        let allow = UNNotificationAction(identifier: Self.allowAction, title: String(localized: "Allow"))
        let deny = UNNotificationAction(identifier: Self.denyAction, title: String(localized: "Deny"), options: [.destructive])
        UNUserNotificationCenter.current().setNotificationCategories([
            UNNotificationCategory(identifier: Self.pairingCategory, actions: [allow, deny], intentIdentifiers: []),
        ])
        for await services in Observations({ owner.route == .local ? owner.thisMac : nil }) {
            guard let services else { continue }
            thisMac = services
            await withTaskGroup { group in
                group.addTask { await self.follow(services) }
                group.addTask { await self.followPairing(services) }
            }
            return
        }
    }

    func open(_ card: String) {
        bringForward()
        openedCard = card
    }

    func bringForward() {
        NSApplication.shared.activate()
        if !NSApplication.shared.windows.contains(where: \.isVisible) { openWindow?(id: "main") }
    }

    /// The first answer wins, here, in the notification or in the TUI. The request leaves the
    /// window at once and comes back only if the Mac did not take the answer.
    func decide(_ id: String, allow: Bool) async {
        guard let thisMac else { return }
        pairingError = nil
        pairingRequests.removeAll { $0.id == id }
        UNUserNotificationCenter.current().removeDeliveredNotifications(withIdentifiers: [Self.identifier(id)])
        do { try await thisMac.decidePairing(id, allow: allow) }
        catch {
            pairingError = String(localized: "The Mac didn’t take that answer. The request may have expired.")
            await refreshPairing(thisMac)
        }
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

    /// A pairing request does not travel on the feed, so this asks the daemon on this Mac every
    /// few seconds; on loopback that costs nothing.
    private func followPairing(_ services: Connection) async {
        while !Task.isCancelled {
            await refreshPairing(services)
            try? await Task.sleep(for: pairingInterval)
        }
    }

    private func refreshPairing(_ services: Connection) async {
        guard let pending = try? await services.pendingPairingRequests() else { return }
        pairingRequests = pending
        let live = Set(pending.map(\.id))
        let gone = notifiedPairings.subtracting(live)
        if !gone.isEmpty {
            UNUserNotificationCenter.current().removeDeliveredNotifications(withIdentifiers: gone.map(Self.identifier))
        }
        dismissedPairings.formIntersection(live)
        notifiedPairings.formIntersection(live)
        for request in pending where !notifiedPairings.contains(request.id) {
            notifiedPairings.insert(request.id)
            await post(request)
        }
    }

    private func post(_ request: PairingRequest) async {
        let content = UNMutableNotificationContent()
        content.title = String(localized: "Allow “\(request.name)” to use Alondra?")
        content.body = String(localized: "Code \(request.code). Allow it only if the device shows the same code.")
        content.categoryIdentifier = Self.pairingCategory
        content.userInfo = ["alondra": ["pairing": request.id]]
        content.interruptionLevel = .timeSensitive
        content.sound = .default
        try? await UNUserNotificationCenter.current().add(
            UNNotificationRequest(identifier: Self.identifier(request.id), content: content, trigger: nil))
    }

    static func identifier(_ id: String) -> String { "pairing-" + id }

    nonisolated static func pairing(of response: UNNotificationResponse) -> String? {
        let content = response.notification.request.content
        guard content.categoryIdentifier == pairingCategory else { return nil }
        return (content.userInfo["alondra"] as? [String: Any])?["pairing"] as? String
    }
}
