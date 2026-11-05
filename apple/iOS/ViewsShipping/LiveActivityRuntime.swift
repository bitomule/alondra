import AlondraLiveActivity
import AlondraNotifications
import Foundation

/// Owns the app's Live Activity controller for the whole process, wakes it when a Mac's grace
/// after reaching zero runs out, and keeps the relay told of this phone's tokens.
@MainActor final class LiveActivityRuntime {
    private var wakeTask: Task<Void, Never>?
    private var startedTask: Task<Void, Never>?
    private var registrationTask: Task<Void, Never>?
    private(set) lazy var controller: LiveActivityController = {
        let keychain = ActivityKeychain()
        return LiveActivityController(host: ActivityKitHost(), open: { keychain?.opened($0) ?? [] }) { [weak self] date in
            self?.wake(at: date)
        }
    }()
    private lazy var registrar: ActivityRegistrar? = {
        guard let group = Bundle.main.object(forInfoDictionaryKey: "AlondraPushKeychainAccessGroup") as? String,
              !group.isEmpty, !group.contains("$(") else { return nil }
        let identity = ActivityPhoneIdentity(accessGroup: group)
        return ActivityRegistrar(phoneID: { try identity.id() }, forgetPhoneID: { try? identity.forget() },
                                 environment: PushCenter.environment.rawValue)
    }()

    /// Listens first thing at launch, so an activity iOS started while the app was closed is seen
    /// and its token reaches the relay.
    func start() {
        startedTask = Task { [weak self] in
            for await _ in ActivityKitHost.started() { self?.controller.reconcile() }
        }
        registrationTask = Task { [weak self] in
            for await event in ActivityKitHost.registrationEvents() { self?.registrar?.handle(event) }
        }
    }

    func enteredForeground() {
        controller.enteredForeground()
        registrar?.retry()
    }

    func relayPairings(_ clients: [NotificationClient]) {
        registrar?.set(clients.map {
            ActivityRelayPairing(id: $0.identity, register: $0.registerActivity, unregister: $0.unregisterActivity)
        })
    }

    private func wake(at date: Date) {
        wakeTask?.cancel()
        wakeTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(max(0, date.timeIntervalSinceNow)))
            guard !Task.isCancelled else { return }
            self?.controller.reconcile()
        }
    }
}
