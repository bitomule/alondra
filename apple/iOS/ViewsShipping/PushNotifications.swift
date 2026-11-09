import AlondraDesign
import AlondraTransport
import AlondraContract
import AlondraAccount
import AlondraNetworkReachability
import AlondraNotifications
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

struct StoredQuietHours: Equatable {
    var enabled: Bool
    var start: Int
    var end: Int

    static func clock(_ minutes: Int) -> String {
        String(format: "%02d:%02d", minutes / 60, minutes % 60)
    }
}

@MainActor
@Observable
final class PushCenter: PushAuthorizing {
    enum Authorization: Equatable {
        case unknown, notDetermined, denied, authorized
    }

    enum Registration: Equatable {
        case idle, sending, registered, failed(String)
    }

    private(set) var authorization = Authorization.unknown
    var isAuthorized: Bool { authorization == .authorized }
    private(set) var registration = Registration.idle
    private(set) var token: String?
    var route: NotificationRoute?
    var quietHours: StoredQuietHours {
        didSet {
            guard quietHours != oldValue else { return }
            let defaults = UserDefaults.standard
            defaults.set(quietHours.enabled, forKey: Keys.quietEnabled)
            defaults.set(quietHours.start, forKey: Keys.quietStart)
            defaults.set(quietHours.end, forKey: Keys.quietEnd)
            scheduleSync()
        }
    }

    private var client: NotificationClient?
    private let routing = NotificationRoutingCoordinator()
    var selectBinding: (@MainActor (NotificationPayload.Binding) async throws -> Void)?
    @ObservationIgnored var onRegistered: (@MainActor () -> Void)?
    private var clients: [NotificationClient] = []
    private let registrations: PushRegistrationCoordinator

    private enum Keys {
        static let quietEnabled = "quietHoursEnabled"
        static let quietStart = "quietHoursStart"
        static let quietEnd = "quietHoursEnd"
    }

    private static let staleNotificationAge: TimeInterval = 10

    init() {
        registrations = PushRegistrationCoordinator(preferences: PushPreferencesLedger(file: AppStorageLocation.applicationSupport(bundleID: "com.bitomule.alondra")
            .appendingPathComponent("push-preferences.json")))
        let defaults = UserDefaults.standard
        quietHours = StoredQuietHours(
            enabled: defaults.bool(forKey: Keys.quietEnabled),
            start: defaults.object(forKey: Keys.quietStart) as? Int ?? 23 * 60,
            end: defaults.object(forKey: Keys.quietEnd) as? Int ?? 8 * 60
        )
        registrations.onChange = { [weak self] state in
            self?.registration = switch state {
            case .idle: .idle
            case .sending: .sending
            case .registered: .registered
            case .failed(let failures): .failed(failures.map(\.message).joined(separator: "\n"))
            }
            if state == .registered { self?.onRegistered?() }
        }
    }

    private var center: UNUserNotificationCenter { .current() }

    func refreshAuthorization() async {
        authorization = switch await center.notificationSettings().authorizationStatus {
        case .notDetermined: .notDetermined
        case .denied: .denied
        default: .authorized
        }
    }

    func activate(whileCurrent isCurrent: @escaping @MainActor () -> Bool = { true }) async {
        guard isCurrent(), !Task.isCancelled else { return }
        do {
            _ = try await center.requestAuthorization(options: [.alert, .sound, .badge])
        } catch {
            guard isCurrent(), !Task.isCancelled else { return }
            registration = .failed(String(localized: "Alerts couldn’t be turned on. Try again in a moment."))
        }
        guard isCurrent(), !Task.isCancelled else { return }
        await refreshAuthorization()
        guard isCurrent(), !Task.isCancelled, authorization == .authorized else { return }
        UIApplication.shared.registerForRemoteNotifications()
    }

    func registerWithAPNs() { UIApplication.shared.registerForRemoteNotifications() }

    /// Apple asks apps to fetch the token again rather than trust an earlier one, and the
    /// relay forgets a token APNs rejected; coming back to the app re-sends it either way.
    func reregisterOnReturn() {
        guard authorization == .authorized, !clients.isEmpty else { return }
        registrations.expireRegistrations()
        registerWithAPNs()
    }

    func didRegister(deviceToken: Data) {
        token = deviceToken.map { String(format: "%02x", $0) }.joined()
        scheduleSync()
    }

    func didFailToRegister(_ error: any Error) {
        registration = .failed(String(localized: "This iPhone couldn’t sign up for alerts. Alondra tries again when you reopen the app."))
    }

    func install(_ entries: [NotificationRoutingCoordinator.Entry], selected: NotificationPayload.Binding?) throws {
        try routing.install(entries, selected: selected)
        let clients = entries.map(\.client)
        self.clients = clients
        self.client = clients.first { $0.binding == selected }
        route = nil
        scheduleSync()
    }

    func clear() {
        clients = []; client = nil; route = nil
        routing.clear(); selectBinding = nil
        registrations.clear()
    }

    func scheduleSync() {
        let quietStart = quietHours.enabled ? StoredQuietHours.clock(quietHours.start) : nil
        let quietEnd = quietHours.enabled ? StoredQuietHours.clock(quietHours.end) : nil
        do {
            try registrations.update(clients: clients, token: token, environment: Self.environment.rawValue,
                quietStart: quietStart, quietEnd: quietEnd)
        } catch {
            registration = .failed((error as? LocalizedError)?.errorDescription
                ?? String(localized: "This iPhone couldn’t sign up for alerts. Alondra tries again when you reopen the app."))
        }
    }

    private func currentBinding() -> NotificationPayload.Binding? { client?.binding }
    var selectedBinding: NotificationPayload.Binding? { currentBinding() }
    func isCurrent(_ route: NotificationRoute) -> Bool { currentBinding() == route.binding }

    /// Simulators and development-signed builds talk to the APNs sandbox; the embedded
    /// provisioning profile says which one a device build was signed for, and App Store
    /// builds carry none. `#if DEBUG` can't tell, because .bazelrc builds with `-c opt`.
    static let environment: PushRegistration.Environment = {
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

    func respond(to response: NotificationResponse) async {
        guard let raw = response.encryptedPayload,
              let authorized = try? routing.authorize(raw) else { return }
        let item = authorized.item
        let binding = authorized.binding
        switch response.action {
        case UNNotificationDefaultActionIdentifier:
            guard await activate(authorized) else { return }
            route = NotificationRoute(itemID: item.card, opensDetail: false, binding: binding)
        case let action where item.opens(action):
            guard await activate(authorized) else { return }
            route = NotificationRoute(itemID: item.card, opensDetail: true, binding: binding)
        default:
            await send(response, authorized: authorized)
        }
    }

    private func activate(_ authorized: NotificationRoutingCoordinator.Authorized) async -> Bool {
        guard let selectBinding else { return false }
        do { try await routing.activate(authorized, select: selectBinding); return true }
        catch { return false }
    }

    private func send(_ response: NotificationResponse, authorized: NotificationRoutingCoordinator.Authorized) async {
        let submission = Task { await routing.submit(authorized, actionID: response.action, text: response.text) }
        let background = UIApplication.shared.beginBackgroundTask(withName: "alondra.notification-action") {
            submission.cancel()
        }
        defer { UIApplication.shared.endBackgroundTask(background) }
        guard let result = await submission.value else { return }
        switch result {
        case .sent(let action):
            switch action.outcome {
            case .applied:
                break
            case .noLongerAvailable:
                await reportNoLongerPending(response)
            case .changed, .rejected:
                await reportFailure(response, reason: action.message ?? String(localized: "This action could not be completed."))
            }
        case .kept:
            await reportKept(response)
        case .uncertain:
            await reportUncertain(response)
        case .failed(let reason):
            await reportFailure(response, reason: reason)
        case .noLongerPending:
            await reportNoLongerPending(response)
        }
    }

    /// An old push can be tapped after the item was decided elsewhere; say so instead of
    /// failing silently.
    private func reportNoLongerPending(_ response: NotificationResponse) async {
        let content = UNMutableNotificationContent()
        content.title = String(localized: "No longer pending")
        content.body = response.title.isEmpty
            ? String(localized: "This was already handled or changed. Open Alondra to see where it stands.")
            : String(localized: "\(response.title) was already handled or changed. Open Alondra to see where it stands.")
        try? await center.add(UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil))
    }

    /// The Mac could not be reached from the background: the answer waits on this device.
    private func reportKept(_ response: NotificationResponse) async {
        let content = UNMutableNotificationContent()
        content.title = String(localized: "Couldn’t reach your Mac")
        content.body = response.title.isEmpty
            ? String(localized: "Your answer will be sent when you open Alondra.")
            : String(localized: "Your answer to \(response.title) will be sent when you open Alondra.")
        try? await center.add(UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil))
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
    private let arrived: @MainActor @Sendable () async -> Void

    init(push: PushCenter, prepare: @escaping @MainActor @Sendable () async -> Void = {},
         arrived: @escaping @MainActor @Sendable () async -> Void = {}) {
        self.push = push
        self.prepare = prepare
        self.arrived = arrived
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
        let isRemotePush = notification.request.trigger is UNPushNotificationTrigger
        if isRemotePush { NetworkReachability.shared.hint() }
        completionHandler(ForegroundPresentation.options(isRemotePush: isRemotePush))
        if isRemotePush { Task { @MainActor [arrived] in await arrived() } }
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
