import AlondraAppComposition
import AlondraNotificationActions
import AlondraPushBinding
import AlondraServicePairing
import CryptoKit
import Foundation
import Observation
import UserNotifications

/// Platform callbacks and key provisioning are process-owned; scene stores stay separate.
@MainActor @Observable
final class ServiceNotificationRuntime {
    let push: PushCenter
    private let bindings = PushBindingStore()
    private let vault: NotificationCredentialVault
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
        vault = NotificationCredentialVault(store: store)
        push = PushCenter(decryptionStore: store)
    }

    func installDelegate() { UNUserNotificationCenter.current().delegate = responder }

    func activate(_ approved: ApprovedServicePairing, services: CloudAppServices) async throws {
        generation += 1
        let current = generation
        identity = ObjectIdentifier(services)
        push.install(nil)
        let signing = try P256.Signing.PrivateKey(rawRepresentation: approved.signingPrivateKey)
        let revision = SHA256.hash(data: signing.publicKey.rawRepresentation + Data(approved.macSigningKey.utf8)
            + Data(approved.ownerSub.utf8) + Data(String(approved.approvedAtMs).utf8))
            .map { String(format: "%02x", $0) }.joined()
        guard let binding = bindings.binding(server: approved.relayURL, deviceID: approved.deviceID,
                                             credentialRevision: revision) else { throw ProvisioningError.binding }
        do {
            try await vault.save(NotificationDecryptionContext(macID: approved.macID, deviceID: approved.deviceID,
                binding: binding.value, macSigningKey: try approved.macSigningPublicKey().rawRepresentation,
                agreementPrivateKey: approved.agreementPrivateKey))
            guard current == generation, !Task.isCancelled else { throw CancellationError() }
            let client = await services.notificationClient(identity: revision, binding: binding)
            guard current == generation, !Task.isCancelled else { throw CancellationError() }
            push.install(client)
            error = nil
            await push.refreshAuthorization()
            if push.authorization == .authorized { push.registerWithAPNs() }
            else if push.authorization == .notDetermined { await push.activate() }
        } catch {
            if current == generation { self.error = error.localizedDescription }
            throw error
        }
    }

    func deactivate(_ services: CloudAppServices?) async throws {
        if let services, let identity, identity != ObjectIdentifier(services) { return }
        generation += 1
        let current = generation
        identity = nil
        push.install(nil)
        bindings.clear()
        UNUserNotificationCenter.current().removeAllDeliveredNotifications()
        UNUserNotificationCenter.current().removeAllPendingNotificationRequests()
        do {
            try await vault.clear()
            if current == generation { error = nil }
        } catch {
            if current == generation { self.error = error.localizedDescription }
            throw error
        }
    }

    /// Main-actor claim delivers a tap once, to whichever active scene is ready first.
    func consumeRoute() -> NotificationRoute? {
        guard let route = push.route, push.isCurrent(route) else { return nil }
        push.route = nil
        return route
    }

    private enum ProvisioningError: LocalizedError {
        case binding
        var errorDescription: String? { "Could not create the notification identity binding" }
    }
}

private actor NotificationCredentialVault {
    let store: NotificationDecryptionStore?
    init(store: NotificationDecryptionStore?) { self.store = store }
    func save(_ context: NotificationDecryptionContext) throws {
        guard let store else { throw ConfigurationError.missingKeychainGroup }
        try store.save(context)
    }
    func clear() throws {
        guard let store else { throw ConfigurationError.missingKeychainGroup }
        try store.clear()
    }
    private enum ConfigurationError: LocalizedError {
        case missingKeychainGroup
        var errorDescription: String? { "The notification shared Keychain group is not configured" }
    }
}
