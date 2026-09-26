import AlondraShared
import AlondraAPIClient
import AlondraCommands
import AlondraNotificationActions
import AlondraNotificationPayload
import Foundation
import Observation
import UIKit
import UserNotifications

typealias NotificationItem = NotificationPayload.Item

struct NotificationRoute: Hashable {
    let itemID: String
    let opensDetail: Bool
    let binding: NotificationPayload.Binding
}

enum NotificationCategory {
    static let question = NotificationPayload.Category.question
    static let permission = NotificationPayload.Category.permission
    static let plan = NotificationPayload.Category.plan
    static let read = NotificationPayload.Category.read

    static let reply = NotificationPayload.Action.reply
    static let moreContext = NotificationPayload.Action.moreContext
    static let allow = NotificationPayload.Action.allow
    static let deny = NotificationPayload.Action.deny
    static let approve = NotificationPayload.Action.approve
    static let review = NotificationPayload.Action.review
    static let markRead = NotificationPayload.Action.markRead
    static let optionPrefix = NotificationPayload.Action.optionPrefix
}

struct NotificationResponse: Sendable {
    let action: String
    let text: String?
    let encryptedPayload: Data?
    let title: String

    init(_ response: UNNotificationResponse) {
        let content = response.notification.request.content
        action = response.actionIdentifier
        text = (response as? UNTextInputNotificationResponse)?.userText
        encryptedPayload = try? JSONSerialization.data(withJSONObject: content.userInfo)
        title = content.title
    }
}

struct QuietHours: Equatable {
    var enabled: Bool
    var start: Int
    var end: Int

    static func clock(_ minutes: Int) -> String {
        String(format: "%02d:%02d", minutes / 60, minutes % 60)
    }
}

@MainActor
@Observable
final class PushCenter {
    enum Authorization: Equatable {
        case unknown, notDetermined, denied, authorized
    }

    enum Registration: Equatable {
        case idle, sending, registered, failed(String)
    }

    private(set) var authorization = Authorization.unknown
    private(set) var registration = Registration.idle
    private(set) var token: String?
    var route: NotificationRoute?
    var quietHours: QuietHours {
        didSet {
            guard quietHours != oldValue else { return }
            let defaults = UserDefaults.standard
            defaults.set(quietHours.enabled, forKey: Keys.quietEnabled)
            defaults.set(quietHours.start, forKey: Keys.quietStart)
            defaults.set(quietHours.end, forKey: Keys.quietEnd)
            scheduleSync()
        }
    }

    private var client: ServiceNotificationClient?
    private let decryptionStore: NotificationDecryptionStore?
    private let preferences: PushPreferencesLedger
    @ObservationIgnored private var lastRegistered: String?
    @ObservationIgnored private var syncTask: Task<Void, Never>?

    private enum Keys {
        static let quietEnabled = "quietHoursEnabled"
        static let quietStart = "quietHoursStart"
        static let quietEnd = "quietHoursEnd"
    }

    private static let staleNotificationAge: TimeInterval = 10

    init(decryptionStore: NotificationDecryptionStore? = nil) {
        self.decryptionStore = decryptionStore
        preferences = PushPreferencesLedger(file: CommandJournalLocation.applicationSupport(bundleID: "com.bitomule.alondra")
            .appendingPathComponent("push-preferences.json"))
        let defaults = UserDefaults.standard
        quietHours = QuietHours(
            enabled: defaults.bool(forKey: Keys.quietEnabled),
            start: defaults.object(forKey: Keys.quietStart) as? Int ?? 23 * 60,
            end: defaults.object(forKey: Keys.quietEnd) as? Int ?? 8 * 60
        )
    }

    private var center: UNUserNotificationCenter { .current() }

    func refreshAuthorization() async {
        authorization = switch await center.notificationSettings().authorizationStatus {
        case .notDetermined: .notDetermined
        case .denied: .denied
        default: .authorized
        }
    }

    func activate() async {
        do {
            _ = try await center.requestAuthorization(options: [.alert, .sound, .badge])
        } catch {
            registration = .failed(error.localizedDescription)
        }
        await refreshAuthorization()
        guard authorization == .authorized else { return }
        UIApplication.shared.registerForRemoteNotifications()
    }

    func registerWithAPNs() { UIApplication.shared.registerForRemoteNotifications() }

    func didRegister(deviceToken: Data) {
        token = deviceToken.map { String(format: "%02x", $0) }.joined()
        scheduleSync()
    }

    func didFailToRegister(_ error: any Error) {
        registration = .failed(error.localizedDescription)
    }

    func install(_ client: ServiceNotificationClient?) {
        self.client = client
        route = nil
        lastRegistered = nil
        scheduleSync()
    }

    func scheduleSync() {
        let previous = syncTask
        previous?.cancel()
        // Serialize idempotent APNs upserts so an older token cannot finish last.
        syncTask = Task {
            await previous?.value
            guard !Task.isCancelled else { return }
            await sync()
        }
    }

    private func sync() async {
        guard let token, let client else { registration = .idle; return }
        let quietStart = quietHours.enabled ? QuietHours.clock(quietHours.start) : nil
        let quietEnd = quietHours.enabled ? QuietHours.clock(quietHours.end) : nil
        let fingerprint = "\(client.identity)|\(client.binding.value)|\(token)|\(Self.environment.rawValue)|\(quietStart ?? "off")|\(quietEnd ?? "off")"
        guard fingerprint != lastRegistered else { return }
        registration = .sending
        do {
            let revision = try await preferences.reserve(identity: client.identity, binding: client.binding.value,
                                                         quietStart: quietStart, quietEnd: quietEnd)
            try Task.checkCancellation()
            do {
                try await client.register(token, Self.environment.rawValue, quietStart, quietEnd, revision)
            } catch ServicePushPreferencesError.stale(let currentRevision) {
                // A restored Keychain identity can outlive its app container. Advance only
                // after the authenticated executor tells us its accepted revision.
                try Task.checkCancellation()
                let newer = try await preferences.reserve(identity: client.identity, binding: client.binding.value,
                    quietStart: quietStart, quietEnd: quietEnd, newerThan: currentRevision)
                try Task.checkCancellation()
                try await client.register(token, Self.environment.rawValue, quietStart, quietEnd, newer)
            }
            guard self.client?.identity == client.identity, self.client?.binding == client.binding,
                  self.token == token, !Task.isCancelled else { return }
            lastRegistered = fingerprint
            registration = .registered
        } catch {
            guard self.client?.identity == client.identity, self.client?.binding == client.binding,
                  !Task.isCancelled else { return }
            registration = .failed(error.localizedDescription)
        }
    }

    private func currentBinding() -> NotificationPayload.Binding? { client?.binding }
    func isCurrent(_ route: NotificationRoute) -> Bool { currentBinding() == route.binding }

    /// Simulators and development-signed builds talk to the APNs sandbox; the embedded
    /// provisioning profile says which one a device build was signed for, and App Store
    /// builds carry none. `#if DEBUG` can't tell, because .bazelrc builds with `-c opt`.
    private static let environment: PushRegistration.Environment = {
        #if targetEnvironment(simulator)
        return .sandbox
        #else
        guard let url = Bundle.main.url(forResource: "embedded", withExtension: "mobileprovision"),
              let profile = try? String(decoding: Data(contentsOf: url), as: UTF8.self),
              let key = profile.range(of: "<key>aps-environment</key>"),
              let value = profile[key.upperBound...].range(of: "<string>development</string>"),
              profile[key.upperBound..<value.lowerBound].trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        else { return .production }
        return .sandbox
        #endif
    }()

    func inboxChanged(pending: [InboxItem]) {
        guard client != nil, authorization == .authorized else { return }
        center.setBadgeCount(pending.count)
        guard let binding = currentBinding() else { return }
        let live = Set(pending.map(\.id))
        let cutoff = Date.now.addingTimeInterval(-Self.staleNotificationAge)
        Task {
            let stale = await center.deliveredNotifications().filter { notification in
                guard notification.date < cutoff,
                      NotificationPayload.Binding(userInfo: notification.request.content.userInfo) == binding,
                      let item = NotificationItem(userInfo: notification.request.content.userInfo) else { return false }
                return !live.contains(item.inboxID)
            }
            guard !stale.isEmpty else { return }
            guard currentBinding() == binding else { return }
            center.removeDeliveredNotifications(withIdentifiers: stale.map(\.request.identifier))
        }
    }

    func respond(to response: NotificationResponse) async {
        guard let raw = response.encryptedPayload,
              let context = try? decryptionStore?.load(), context.binding == currentBinding()?.value,
              let decrypted = try? ServiceNotificationDecoder.decrypt(raw, context: context,
                  nowMilliseconds: UInt64(Date.now.timeIntervalSince1970 * 1000)),
              let payload = (try? JSONSerialization.jsonObject(with: decrypted)) as? [AnyHashable: Any],
              let boundItem = NotificationPayload.BoundItem(userInfo: payload),
              boundItem.matches(currentBinding()) else { return }
        let item = boundItem.item
        let binding = boundItem.binding
        switch response.action {
        case UNNotificationDefaultActionIdentifier:
            route = NotificationRoute(itemID: item.inboxID, opensDetail: item.kind != .question, binding: binding)
        case NotificationCategory.review:
            route = NotificationRoute(itemID: item.inboxID, opensDetail: true, binding: binding)
        default:
            guard let command = NotificationAction(item: item, action: response.action, text: response.text) else { return }
            await send(response, itemID: item.inboxID, binding: binding, command: command)
        }
    }

    private func send(_ response: NotificationResponse, itemID: String, binding: NotificationPayload.Binding, command: NotificationAction) async {
        guard let client, client.binding == binding else { return }
        let submission = Task { await client.submit(command, itemID) }
        let background = UIApplication.shared.beginBackgroundTask(withName: "alondra.notification-action") {
            submission.cancel()
        }
        defer { UIApplication.shared.endBackgroundTask(background) }
        let result = await submission.value
        guard currentBinding() == binding else { return }
        switch result {
        case .confirmed:
            break
        case .unknown:
            await reportUncertain(response)
        case .rejected(let error):
            await reportFailure(response, reason: error.friendlyDescription)
        case .obsolete:
            return
        }
    }

    private func reportUncertain(_ response: NotificationResponse) async {
        let content = UNMutableNotificationContent()
        content.title = String(localized: "Check Alondra")
        content.body = response.title.isEmpty
            ? String(localized: "The action may have completed. Check its current state before trying again.")
            : String(localized: "\(response.title) may have completed. Check its current state before trying again.")
        content.sound = .default
        try? await center.add(UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil))
    }

    private func reportFailure(_ response: NotificationResponse, reason: String) async {
        let content = UNMutableNotificationContent()
        content.title = String(localized: "Couldn’t send")
        content.body = response.title.isEmpty ? reason : "\(response.title) — \(reason)"
        content.sound = .default
        // A failed send is informational. Reusing its category or payload would
        // expose actionable buttons that could target a different pairing.
        try? await center.add(UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil))
    }


}

/// UserNotifications calls its delegate off the main thread, and on iOS 26 the completion
/// handler must run on the main thread, so the handler is carried across to it explicitly.
final class NotificationResponder: NSObject, UNUserNotificationCenterDelegate {
    private let push: PushCenter
    private let prepare: @MainActor @Sendable () async -> Void

    init(push: PushCenter, prepare: @escaping @MainActor @Sendable () async -> Void = {}) {
        self.push = push
        self.prepare = prepare
    }

    func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse, withCompletionHandler completionHandler: @escaping () -> Void) {
        let response = NotificationResponse(response)
        let completion = MainThreadCompletion(completionHandler)
        Task { @MainActor [push, prepare] in
            await prepare()
            await push.respond(to: response)
            completion.run()
        }
    }

    func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification, withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void) {
        completionHandler([.banner, .list, .badge])
    }
}

/// Invariant: `run()` is only called once, from the main actor.
private struct MainThreadCompletion: @unchecked Sendable {
    private let handler: () -> Void

    init(_ handler: @escaping () -> Void) {
        self.handler = handler
    }

    @MainActor func run() {
        handler()
    }
}
