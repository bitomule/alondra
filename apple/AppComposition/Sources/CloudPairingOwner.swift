import AlondraWorkspace
import AlondraAuth
import AlondraServicePairing
import AlondraCloudPairingFeature
import ComposableArchitecture
import Observation
import Foundation

/// Process-owned credential and pairing use cases. Secrets never enter presentation state.
@MainActor
@Observable
public final class CloudPairingOwner {
    public let auth: AuthSession
    private let displayName: String
    private let storageDirectory: URL
    private let prepareCamera: @Sendable () async -> Bool
    private let didReconcile: @MainActor (ApprovedSessionRegistry<CloudAppServices>.Snapshot) async throws -> (@MainActor () -> Void)
    private let willDeactivate: @MainActor () async throws -> Void
    @ObservationIgnored private lazy var registry = ApprovedSessionRegistry<CloudAppServices>(
        make: { [auth, storageDirectory] in try CloudAppServices(approved: $0, auth: auth, storageDirectory: storageDirectory) },
        stop: { await $0.deactivate() }, publish: { [weak self] snapshot in
            guard let self else { throw CancellationError() }
            return try await self.didReconcile(snapshot)
        }, clearPlatform: { [weak self] in try await self?.willDeactivate() })
    private let pairing = ServicePairingClient()
    private var restoreTask: Task<Bool, Error>?
    private var restoreTaskID: UUID?
    private var attempt: UUID?
    private var generation = 0
    public private(set) var services: CloudAppServices?
    @ObservationIgnored public lazy var store = Store(initialState: CloudPairingFeature.State()) { CloudPairingFeature() } withDependencies: {
        $0.cloudPairing = CloudPairingService(
            restore: { [self] in try await restore() }, login: { [self] in try await login() },
            begin: { [self] payload in try await begin(payload) }, approve: { [self] in try await approve() },
            cancel: { [self] in await cancel() }, logout: { [self] in try await logout() })
        $0.cloudPairing.prepareCamera = self.prepareCamera
        $0.cloudPairing.hasSavedConfiguration = { [self] in
            if try await auth.requiresRevocation() { return true }
            return try await pairing.approvedPairing() != nil
        }
    }
    public init(auth: AuthSession, displayName: String, storageDirectory: URL,
                prepareCamera: @escaping @Sendable () async -> Bool = { false },
                didReconcile: @escaping @MainActor (ApprovedSessionRegistry<CloudAppServices>.Snapshot) async throws -> (@MainActor () -> Void) = { _ in {} },
                willDeactivate: @escaping @MainActor () async throws -> Void = {}) {
        self.auth = auth
        self.displayName = displayName
        self.storageDirectory = storageDirectory
        self.prepareCamera = prepareCamera
        self.didReconcile = didReconcile
        self.willDeactivate = willDeactivate
    }
    /// A QR may name the relay but cannot authorize a destination for the account bearer.
    private static func validateRelay(_ url: URL) throws {
        guard url.scheme == "https", url.host()?.lowercased() == "alondra-api.davidcollado.dev",
              url.port == nil || url.port == 443, url.user == nil, url.password == nil,
              url.query == nil, url.fragment == nil, url.path.isEmpty || url.path == "/" else {
            throw ServicePairingError.invalidInvitation
        }
    }
    public func prepareNotificationSession() async throws {
        if services == nil { _ = try await restore() }
    }
    private func restore() async throws -> Bool {
        if services != nil { return true }
        if let restoreTask { return try await restoreTask.value }
        let id = UUID()
        let task = Task { try await self.restoreApprovedPairing() }
        restoreTask = task
        restoreTaskID = id
        defer { if restoreTaskID == id { restoreTask = nil; restoreTaskID = nil } }
        return try await task.value
    }
    private func restoreApprovedPairing() async throws -> Bool {
        let current = generation
        do { _ = try await auth.hasSession() }
        catch AuthError.pendingRevocation {
            throw CloudRestoreError.revocationRequired(AuthError.pendingRevocation.localizedDescription)
        } catch AuthError.rotationUncertain {
            throw CloudRestoreError.revocationRequired(AuthError.rotationUncertain.localizedDescription)
        }
        let collection = try await pairing.approvedPairings()
        guard !collection.pairings.isEmpty else { return false }
        let subject: String
        do { subject = try await auth.verifiedSubject() }
        catch AuthError.noSession { throw CloudRestoreError.reauthenticationRequired("Sign in again to reconnect your saved workspace.") }
        let authorized = collection.pairings.filter { $0.ownerSub == subject }
        guard !authorized.isEmpty else {
            throw CloudRestoreError.reauthenticationRequired("Sign in with the account that paired this workspace.")
        }
        for approved in authorized { try Self.validateRelay(approved.relayURL) }
        try Task.checkCancellation()
        guard current == generation else { throw CancellationError() }
        try await reconcile(collection, subject: subject, preferred: collection.selectedID, generation: current)
        return true
    }
    private func login() async throws {
        try await deactivate()
        try await auth.login()
        _ = try await auth.verifiedSubject()
    }
    private func begin(_ payload: String) async throws -> String {
        let invitation = try ServicePairingInvitation(qrPayload: payload)
        try Self.validateRelay(invitation.url)
        let current = generation
        let subject = try await auth.verifiedSubject()
        let token = try await auth.bearerToken()
        let pending = try await pairing.begin(qrPayload: payload,
            authorization: PairingAuthorization(bearerToken: token, ownerSub: subject), displayName: displayName)
        guard current == generation, !Task.isCancelled else {
            await pairing.discard(pending.attemptID)
            throw CancellationError()
        }
        attempt = pending.attemptID
        return pending.comparisonCode
    }
    private func approve() async throws {
        guard let attempt else { throw ServicePairingError.missingAttempt }
        let current = generation
        let approved: ApprovedServicePairing
        do { approved = try await pairing.waitForApproval(attempt) }
        catch {
            await pairing.discard(attempt)
            if self.attempt == attempt { self.attempt = nil }
            throw error
        }
        guard current == generation, !Task.isCancelled else { throw CancellationError() }
        self.attempt = nil
        let subject = try await auth.verifiedSubject()
        guard current == generation, subject == approved.ownerSub else { throw CancellationError() }
        let collection = try await pairing.approvedPairings()
        try await reconcile(collection, subject: subject, preferred: ApprovedPairingID(approved), generation: current)
    }
    private func cancel() async {
        generation += 1
        if let attempt { self.attempt = nil; await pairing.discard(attempt) }
    }
    public func selectMac(_ id: ApprovedPairingID) async throws {
        let current = generation
        let subject = try await auth.verifiedSubject()
        guard current == generation, id.ownerSub == subject else { throw CancellationError() }
        try Self.validateRelay(id.relayURL)
        try await pairing.selectApprovedPairing(id)
        guard current == generation else { throw CancellationError() }
        try await registry.select(id, verifiedSubject: subject)
        guard current == generation else { throw CancellationError() }
        services = registry.snapshot.selected?.session
    }

    private func reconcile(_ collection: ApprovedPairingCollection, subject: String,
                           preferred: ApprovedPairingID?, generation current: Int) async throws {
        let authorized = collection.pairings.filter { $0.ownerSub == subject }
        for approved in authorized { try Self.validateRelay(approved.relayURL) }
        guard current == generation, !Task.isCancelled else { throw CancellationError() }
        try await registry.reconcile(authorized, verifiedSubject: subject, preferred: preferred)
        guard current == generation, !Task.isCancelled else { throw CancellationError() }
        for entry in registry.snapshot.entries {
            await entry.session.startRecovery()
            guard current == generation, !Task.isCancelled else { throw CancellationError() }
        }
        services = registry.snapshot.selected?.session
    }

    private func deactivate() async throws {
        generation += 1
        restoreTask?.cancel()
        restoreTask = nil
        restoreTaskID = nil
        attempt = nil
        services = nil
        await pairing.discardAllAttempts()
        try await registry.clear()
    }
    private func logout() async throws {
        // Attempt every revocation even when notification/keychain cleanup fails.
        var failures: [String] = []
        do { try await deactivate() } catch { failures.append(error.localizedDescription) }
        do {
            try await auth.logout()
            try await pairing.removeApprovedPairing()
        } catch { failures.append(error.localizedDescription) }
        if !failures.isEmpty { throw CleanupFailure(details: failures.joined(separator: "\n")) }
    }
    private struct CleanupFailure: LocalizedError {
        let details: String
        var errorDescription: String? { details }
    }
}
