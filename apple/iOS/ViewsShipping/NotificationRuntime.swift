import AlondraDesign
import AlondraAccount
import AlondraNotifications
import AlondraPairing
import CryptoKit
import Foundation
import Observation
import UserNotifications

/// Platform callbacks and key provisioning are process-owned; scene stores stay separate.
@MainActor @Observable
final class NotificationRuntime {
    let push: PushCenter
    private let bindings = PushBindingStore()
    private let vault: NotificationDecryptionStore?
    @ObservationIgnored var selectSession: (@MainActor (ApprovedPairingID) async throws -> Void)?
    @ObservationIgnored var prepareSession: (@MainActor () async throws -> Void)?
    @ObservationIgnored private lazy var responder = NotificationResponder(push: push, prepare: { [weak self] in
        guard let self else { return }
        do { try await prepareSession?() }
        catch { self.error = error.localizedDescription }
    })
    private var identity: ObjectIdentifier?
    private var generation = 0
    private(set) var error: String?
    var statusError: String? {
        if let error { return error }
        if case .failed(let message) = push.registration { return message }
        return nil
    }

    init() {
        let group = Bundle.main.object(forInfoDictionaryKey: "AlondraPushKeychainAccessGroup") as? String
        let store = group.flatMap { $0.isEmpty || $0.contains("$(") ? nil : NotificationDecryptionStore(accessGroup: $0) }
        vault = store
        push = PushCenter()
    }

    func owns(_ services: Connection) -> Bool { identity == ObjectIdentifier(services) }

    func installDelegate() { UNUserNotificationCenter.current().delegate = responder }

    func prepare(_ snapshot: ApprovedSessionRegistry<Connection>.Snapshot) async throws -> (@MainActor () throws -> Void) {
        generation += 1
        let current = generation
        var contexts: [NotificationDecryptionContext] = []
        var clients: [NotificationClient] = []
        var selected: NotificationPayload.Binding?
        var destinations: [NotificationPayload.Binding: ApprovedPairingID] = [:]
        for entry in snapshot.entries {
            let approved = entry.approved
            let signing = try P256.Signing.PrivateKey(rawRepresentation: approved.signingPrivateKey)
            let revision = SHA256.hash(data: signing.publicKey.rawRepresentation + Data(approved.macSigningKey.utf8)
                + Data(approved.ownerSub.utf8) + Data(String(approved.approvedAtMs).utf8))
                .map { String(format: "%02x", $0) }.joined()
            let scope = NotificationBindingScope(relayURL: approved.relayURL, ownerSub: approved.ownerSub,
                macID: approved.macID, deviceID: approved.deviceID, credentialRevision: revision)
            guard let binding = bindings.binding(scope: scope) else { throw ProvisioningError.binding }
            contexts.append(NotificationDecryptionContext(macID: approved.macID, deviceID: approved.deviceID,
                binding: binding.value, macSigningKey: try approved.macSigningPublicKey().rawRepresentation,
                agreementPrivateKey: approved.agreementPrivateKey, scope: scope))
            let client = await entry.session.notificationClient(identity: revision, binding: binding)
            guard current == generation, !Task.isCancelled else { throw CancellationError() }
            clients.append(client)
            destinations[binding] = entry.id
            if entry.id == snapshot.selectedID { selected = binding }
        }
        guard Set(clients.map(\.identity)).count == clients.count,
              Set(clients.map(\.binding)).count == clients.count else { throw ProvisioningError.binding }
        do {
            await push.refreshAuthorization()
            guard current == generation, !Task.isCancelled else { throw CancellationError() }
            if push.authorization == .notDetermined { await push.activate() }
            guard current == generation, !Task.isCancelled else { throw CancellationError() }
            guard let vault else { throw ProvisioningError.missingKeychainGroup }
            let routes = zip(clients, contexts).map { NotificationRoutingCoordinator.Entry(client: $0.0, context: $0.1) }
            // Validate the routing snapshot before the atomic vault/publication commit.
            try NotificationRoutingCoordinator().install(routes, selected: selected)
            return { [self, routes, selected, contexts, destinations] in
                guard current == generation, !Task.isCancelled else { throw CancellationError() }
                // Keychain mutation and platform/session publication are one synchronous
                // MainActor commit. A stale preparation cannot repopulate keys after logout.
                try vault.replaceAuthorizedContexts(contexts)
                identity = snapshot.selected.map { ObjectIdentifier($0.session) }
                try push.install(routes, selected: selected)
                push.selectBinding = { [weak self, destinations] binding in
                    guard let self, let id = destinations[binding], let select = self.selectSession else { throw CancellationError() }
                    try await select(id)
                }
                error = nil
                if push.authorization == .authorized { push.registerWithAPNs() }
            }
        } catch {
            if current == generation { self.error = error.localizedDescription }
            throw error
        }
    }

    func deactivate() async throws {
        generation += 1
        let current = generation
        identity = nil
        push.clear()
        bindings.clear()
        UNUserNotificationCenter.current().removeAllDeliveredNotifications()
        UNUserNotificationCenter.current().removeAllPendingNotificationRequests()
        do {
            guard let vault else { throw ProvisioningError.missingKeychainGroup }
            try vault.clear()
            if current == generation { error = nil }
        } catch {
            if current == generation { self.error = error.localizedDescription }
            throw error
        }
    }

    /// Main-actor claim delivers a tap once, to whichever active scene is ready first.
    func consumeRoute(for services: Connection) -> NotificationRoute? {
        guard owns(services), let route = push.route, push.isCurrent(route) else { return nil }
        push.route = nil
        return route
    }

    private enum ProvisioningError: LocalizedError {
        case binding, missingKeychainGroup
        var errorDescription: String? {
            switch self {
            case .binding: "Could not create the notification identity binding"
            case .missingKeychainGroup: "The notification shared Keychain group is not configured"
            }
        }
    }
}
