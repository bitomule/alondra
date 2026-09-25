import AlondraShared
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
    static let questionPrefix = NotificationPayload.Category.questionPrefix


}

struct NotificationResponse: Sendable {
    let action: String
    let text: String?
    let item: NotificationItem?
    let title: String
    let category: String
    let thread: String
    let payload: Data?

    init(_ response: UNNotificationResponse) {
        let content = response.notification.request.content
        action = response.actionIdentifier
        text = (response as? UNTextInputNotificationResponse)?.userText
        item = NotificationItem(userInfo: content.userInfo)
        title = content.title
        category = content.categoryIdentifier
        thread = content.threadIdentifier
        payload = (content.userInfo["alondra"] as? [String: Any]).flatMap { try? JSONSerialization.data(withJSONObject: $0) }
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

    private let fleet: FleetStore
    private let pairing: DevicePairing
    @ObservationIgnored private var lastRegistered: String?
    @ObservationIgnored private var syncTask: Task<Void, Never>?

    private enum Keys {
        static let quietEnabled = "quietHoursEnabled"
        static let quietStart = "quietHoursStart"
        static let quietEnd = "quietHoursEnd"
    }

    private static let actionDeadline: Duration = .seconds(22)
    private static let staleNotificationAge: TimeInterval = 10

    init(fleet: FleetStore, pairing: DevicePairing) {
        self.fleet = fleet
        self.pairing = pairing
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

    func didRegister(deviceToken: Data) {
        token = deviceToken.map { String(format: "%02x", $0) }.joined()
        scheduleSync()
    }

    func didFailToRegister(_ error: any Error) {
        registration = .failed(error.localizedDescription)
    }

    func scheduleSync() {
        syncTask?.cancel()
        syncTask = Task { await sync() }
    }

    private func sync() async {
        guard let token, pairing.isPaired, let client = currentClient() else { return }
        let request = PushRegistration(
            token: token,
            environment: Self.environment,
            quietStart: quietHours.enabled ? QuietHours.clock(quietHours.start) : nil,
            quietEnd: quietHours.enabled ? QuietHours.clock(quietHours.end) : nil
        )
        let fingerprint = "\(client.identity)|\(request)"
        guard fingerprint != lastRegistered else { return }
        registration = .sending
        do {
            try await client.registerPush(request)
            lastRegistered = fingerprint
            registration = .registered
        } catch where error.isCancellation {
            return
        } catch {
            registration = .failed(error.friendlyDescription)
        }
    }

    private func currentClient() -> AlondraClient? {
        let address = UserDefaults.standard.string(forKey: ServerSetting.key) ?? ServerSetting.defaultURL
        guard let url = AlondraClient.serverURL(address), let signer = pairing.signer(for: url) else { return nil }
        return AlondraClient(base: url, device: "iphone", signer: signer)
    }

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
        guard fleet.connection == .online, authorization == .authorized else { return }
        center.setBadgeCount(pending.count)
        let live = Set(pending.map(\.id))
        let cutoff = Date.now.addingTimeInterval(-Self.staleNotificationAge)
        Task {
            let stale = await center.deliveredNotifications().filter { notification in
                guard notification.date < cutoff, let item = NotificationItem(userInfo: notification.request.content.userInfo) else { return false }
                return !live.contains(item.inboxID)
            }
            guard !stale.isEmpty else { return }
            center.removeDeliveredNotifications(withIdentifiers: stale.map(\.request.identifier))
        }
    }

    func respond(to response: NotificationResponse) async {
        guard let item = response.item else { return }
        switch response.action {
        case UNNotificationDefaultActionIdentifier:
            route = NotificationRoute(itemID: item.inboxID, opensDetail: item.kind != .question)
        case NotificationCategory.review:
            route = NotificationRoute(itemID: item.inboxID, opensDetail: true)
        default:
            guard let command = NotificationAction(item: item, action: response.action, text: response.text) else { return }
            await send(response) { try await command.execute(using: $0) }
        }
    }

    private func send(_ response: NotificationResponse, _ action: @escaping @Sendable (AlondraClient) async throws -> Void) async {
        guard let client = currentClient() else {
            await reportFailure(response, reason: String(localized: "No server configured"))
            return
        }
        let background = UIApplication.shared.beginBackgroundTask(withName: "alondra.notification-action")
        defer { UIApplication.shared.endBackgroundTask(background) }
        do {
            try await Self.withDeadline(Self.actionDeadline) { try await action(client) }
            if UIApplication.shared.applicationState == .active {
                await fleet.refresh(ifCurrentClientMatches: client.identity)
            }
        } catch {
            await reportFailure(response, reason: error.friendlyDescription)
        }
    }

    private func reportFailure(_ response: NotificationResponse, reason: String) async {
        let content = UNMutableNotificationContent()
        content.title = String(localized: "Couldn’t send")
        content.body = response.title.isEmpty ? reason : "\(response.title) — \(reason)"
        content.sound = .default
        content.categoryIdentifier = response.category
        content.threadIdentifier = response.thread
        if let payload = response.payload, let alondra = try? JSONSerialization.jsonObject(with: payload) {
            content.userInfo = ["alondra": alondra]
        }
        try? await center.add(UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil))
    }

    private static func withDeadline(_ deadline: Duration, _ work: @escaping @Sendable () async throws -> Void) async throws {
        try await withThrowingTaskGroup(of: Void.self) { group in
            group.addTask { try await work() }
            group.addTask {
                try await Task.sleep(for: deadline)
                throw URLError(.timedOut)
            }
            try await group.next()
            group.cancelAll()
        }
    }
}

/// UserNotifications calls its delegate off the main thread, and on iOS 26 the completion
/// handler must run on the main thread, so the handler is carried across to it explicitly.
final class NotificationResponder: NSObject, UNUserNotificationCenterDelegate {
    private let push: PushCenter

    init(push: PushCenter) {
        self.push = push
    }

    func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse, withCompletionHandler completionHandler: @escaping () -> Void) {
        let response = NotificationResponse(response)
        let completion = MainThreadCompletion(completionHandler)
        Task { @MainActor [push] in
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
