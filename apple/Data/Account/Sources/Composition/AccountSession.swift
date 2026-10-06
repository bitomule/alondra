import AlondraNetworkReachability
import AlondraPairing
import AlondraFeatures
import ComposableArchitecture
import Observation
import Foundation

/// Process-owned credential and pairing use cases. Secrets never enter presentation state.
@MainActor
@Observable
public final class AccountSession {
    public let auth: AuthSession
    private let displayName: String
    private let storageDirectory: URL
    private let prepareCamera: @Sendable () async -> Bool
    private let didReconcile: @MainActor (ApprovedSessionRegistry<Connection>.Snapshot) async throws -> (@MainActor () throws -> Void)
    private let willDeactivate: @MainActor () async throws -> Void
    @ObservationIgnored private var storedRegistry: ApprovedSessionRegistry<Connection>?
    private var registry: ApprovedSessionRegistry<Connection> {
        if let storedRegistry { return storedRegistry }
        let created = makeRegistry()
        storedRegistry = created
        return created
    }
    private func makeRegistry() -> ApprovedSessionRegistry<Connection> {
        let auth = auth
        return ApprovedSessionRegistry<Connection>(
            make: { try Connection(approved: $0, auth: auth) },
            stop: { await $0.deactivate() }, publish: { [weak self] snapshot in
                guard let self else { throw CancellationError() }
                return try await self.didReconcile(snapshot)
            }, clearPlatform: { [weak self] in try await self?.willDeactivate() })
    }
    private let pairing = PairingClient()
    private var restoreTask: Task<Bool, Error>?
    private var restoreTaskID: UUID?
    private var attempt: UUID?
    private let attempts = PairingAttemptLease()
    private var generation = 0
    private var choiceIDs: [ApprovedPairingID: UUID] = [:]
    private var selection: UUID?
    public private(set) var macs: [MacChoice] = []
    public private(set) var selectedMacID: UUID?
    public private(set) var services: Connection?
    @ObservationIgnored public lazy var store = Store(initialState: SignInFeature.State()) { SignInFeature() } withDependencies: {
        $0.cloudPairing = SignInClient(
            restore: { [self] in try await restore() }, login: { [self] in try await login() },
            begin: { [self] payload in try await begin(payload) }, approve: { [self] in try await approve() },
            cancel: { [self] in await cancel() }, logout: { [self] in try await logout() })
        $0.cloudPairing.prepareCamera = self.prepareCamera
        $0.cloudPairing.classify = {
            ConnectionClassifier.problem(for: $0, local: false, networkOffline: NetworkReachability.shared.isOffline)
        }
        $0.cloudPairing.hasSavedConfiguration = { [self] in
            if try await auth.requiresRevocation() { return true }
            return try await pairing.approvedPairing() != nil
        }
    }
    public init(auth: AuthSession, displayName: String, storageDirectory: URL,
                prepareCamera: @escaping @Sendable () async -> Bool = { false },
                didReconcile: @escaping @MainActor (ApprovedSessionRegistry<Connection>.Snapshot) async throws -> (@MainActor () throws -> Void) = { _ in {} },
                willDeactivate: @escaping @MainActor () async throws -> Void = {}) {
        self.auth = auth
        self.displayName = displayName
        self.storageDirectory = storageDirectory
        self.prepareCamera = prepareCamera
        self.didReconcile = didReconcile
        self.willDeactivate = willDeactivate
    }
    public var additionalMacPairing: SignInClient {
        let scope = UUID()
        let expectedSession = services
        var service = SignInClient(restore: { throw CancellationError() }, login: { throw CancellationError() },
            begin: { [self] payload in
                guard let expectedSession else { throw CancellationError() }
                return try await begin(payload, scope: scope, expectedSession: expectedSession)
            }, approve: { [self] in
                guard let expectedSession else { throw CancellationError() }
                try await approve(scope: scope, expectedSession: expectedSession)
            },
            cancel: { [self] in await cancel(scope: scope) }, logout: { throw CancellationError() })
        service.prepareCamera = prepareCamera
        return service
    }

    /// A QR may name the relay but cannot authorize a destination for the account bearer.
    private static func validateRelay(_ url: URL) throws {
        guard url.scheme == "https", url.host()?.lowercased() == "alondra-api.davidcollado.dev",
              url.port == nil || url.port == 443, url.user == nil, url.password == nil,
              url.query == nil, url.fragment == nil, url.path.isEmpty || url.path == "/" else {
            throw PairingError.invalidInvitation
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
            throw RestoreError.revocationRequired(AuthError.pendingRevocation.localizedDescription)
        } catch AuthError.rotationUncertain {
            throw RestoreError.revocationRequired(AuthError.rotationUncertain.localizedDescription)
        }
        let collection = try await pairing.approvedPairings()
        guard !collection.pairings.isEmpty else { return false }
        let subject: String
        do { subject = try await auth.verifiedSubject() }
        catch AuthError.noSession { throw RestoreError.reauthenticationRequired("Sign in again to reconnect your saved workspace.") }
        let authorized = collection.pairings.filter { $0.ownerSub == subject }
        guard !authorized.isEmpty else {
            throw RestoreError.reauthenticationRequired("Sign in with the account that paired this workspace.")
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
    private func begin(_ payload: String, scope: UUID? = nil, expectedSession: Connection? = nil) async throws -> String {
        if let expectedSession { guard services === expectedSession else { throw CancellationError() } }
        let ticket = try attempts.acquire(scope: scope)
        var retained = false
        defer { if !retained { attempts.release(ticket) } }
        let invitation = try PairingInvitation(qrPayload: payload)
        try Self.validateRelay(invitation.url)
        let current = generation
        let subject = try await auth.verifiedSubject()
        let token = try await auth.bearerToken()
        let pending = try await pairing.begin(qrPayload: payload,
            authorization: PairingAuthorization(bearerToken: token, ownerSub: subject), displayName: displayName,
            rejectExistingMac: scope != nil)
        guard current == generation, attempts.contains(ticket), !Task.isCancelled,
              expectedSession == nil || services === expectedSession else {
            await pairing.discard(pending.attemptID)
            throw CancellationError()
        }
        attempt = pending.attemptID
        retained = true
        return pending.comparisonCode
    }
    private func approve(scope: UUID? = nil, expectedSession: Connection? = nil) async throws {
        if let expectedSession { guard services === expectedSession else { throw CancellationError() } }
        guard let ticket = attempts.current(scope: scope), let attempt else { throw PairingError.missingAttempt }
        defer { attempts.release(ticket) }
        let current = generation
        let approved: ApprovedServicePairing
        do { approved = try await pairing.waitForApproval(attempt) }
        catch {
            await pairing.discard(attempt)
            if self.attempt == attempt { self.attempt = nil }
            throw error
        }
        guard current == generation, attempts.contains(ticket), !Task.isCancelled,
              expectedSession == nil || services === expectedSession else { throw CancellationError() }
        self.attempt = nil
        let subject = try await auth.verifiedSubject()
        guard current == generation, subject == approved.ownerSub else { throw CancellationError() }
        let collection = try await pairing.approvedPairings()
        guard expectedSession == nil || services === expectedSession else { throw CancellationError() }
        try await reconcile(collection, subject: subject, preferred: ApprovedPairingID(approved), generation: current)
    }
    private func cancel(scope: UUID? = nil) async {
        if let ticket = attempts.current(scope: scope) { attempts.release(ticket) }
        else if scope != nil || attempts.hasPending { return }
        generation += 1
        if let attempt { self.attempt = nil; await pairing.discard(attempt) }
    }
    public func selectMac(choice id: UUID, expectedSession: Connection) async throws {
        try await SessionBoundAction.run(expected: expectedSession, current: { self.services }) { expected in
            guard let pairingID = self.choiceIDs.first(where: { $0.value == id })?.key else { throw CancellationError() }
            try await self.selectMac(pairingID, expectedSession: expected)
        }
    }

    public func selectMac(_ id: ApprovedPairingID, expectedSession: Connection? = nil) async throws {
        if let expectedSession { guard services === expectedSession else { throw CancellationError() } }
        guard selection == nil else { throw CancellationError() }
        let operation = UUID()
        selection = operation
        defer { if selection == operation { selection = nil } }
        let current = generation
        let subject = try await auth.verifiedSubject()
        guard current == generation, id.ownerSub == subject else { throw CancellationError() }
        try Self.validateRelay(id.relayURL)
        let saved = try await pairing.approvedPairings()
        guard current == generation,
              let entry = registry.snapshot.entries.first(where: { $0.id == id }),
              let approved = saved.pairings.first(where: { ApprovedPairingID($0) == id }),
              approved.signingPrivateKey == entry.approved.signingPrivateKey,
              approved.agreementPrivateKey == entry.approved.agreementPrivateKey,
              approved.macSigningKey == entry.approved.macSigningKey,
              approved.macAgreementKey == entry.approved.macAgreementKey else { throw CancellationError() }
        // A saved pairing is not proof that access has not been revoked remotely.
        // Select only after a signed round trip through this exact Mac's session.
        try await entry.session.checkSettingsConnection()
        guard current == generation, !Task.isCancelled,
              expectedSession == nil || services === expectedSession else { throw CancellationError() }
        do {
            try await pairing.selectApprovedPairing(id)
            guard current == generation, !Task.isCancelled,
                  expectedSession == nil || services === expectedSession else { throw CancellationError() }
            try await registry.select(id, verifiedSubject: subject)
            guard current == generation, registry.snapshot.selectedID == id else { throw CancellationError() }
            // The registry's synchronous commit is the acceptance point. Cancellation
            // after it must not roll back only the preference and leave the vault on B.
            publishChoices()
        } catch {
            // No suspension between the owner-generation check and the store CAS:
            // logout cannot interleave and resurrect a preference from this action.
            if current == generation, let active = registry.snapshot.selectedID {
                // Another approval may have published while persistence suspended.
                // Restore the live registry selection, never a captured older one.
                do { try pairing.restoreApprovedSelection(active, ifSelected: id) }
                catch { throw CleanupFailure(details: "Could not restore the previous Mac selection: " + error.localizedDescription) }
            }
            throw error
        }
    }

    private func reconcile(_ collection: ApprovedPairingCollection, subject: String,
                           preferred: ApprovedPairingID?, generation current: Int) async throws {
        let authorized = collection.pairings.filter { $0.ownerSub == subject }
        for approved in authorized { try Self.validateRelay(approved.relayURL) }
        guard current == generation, !Task.isCancelled else { throw CancellationError() }
        try await registry.reconcile(authorized, verifiedSubject: subject, preferred: preferred)
        guard current == generation, !Task.isCancelled else { throw CancellationError() }
        publishChoices()
    }

    private func publishChoices() {
        let snapshot = registry.snapshot
        choiceIDs = Dictionary(uniqueKeysWithValues: snapshot.entries.map { ($0.id, choiceIDs[$0.id] ?? UUID()) })
        macs = snapshot.entries.compactMap { entry in
            choiceIDs[entry.id].map { MacChoice(id: $0, macID: entry.id.macID) }
        }.sorted { $0.macID < $1.macID }
        selectedMacID = snapshot.selectedID.flatMap { choiceIDs[$0] }
        services = snapshot.selected?.session
    }

    private func deactivate() async throws {
        generation += 1
        restoreTask?.cancel()
        restoreTask = nil
        restoreTaskID = nil
        attempt = nil
        attempts.clear()
        services = nil
        macs = []; selectedMacID = nil; choiceIDs = [:]; selection = nil
        await pairing.discardAllAttempts()
        try await registry.clear()
    }
    private func logout() async throws {
        // Attempt every revocation even when notification/keychain cleanup fails.
        var failures: [String] = []
        await removePushBindings()
        do { try await deactivate() } catch { failures.append(error.localizedDescription) }
        do {
            try await auth.logout()
            try await pairing.removeApprovedPairing()
        } catch { failures.append(error.localizedDescription) }
        if !failures.isEmpty { throw CleanupFailure(details: failures.joined(separator: "\n")) }
    }
    /// Best effort while the sessions can still reach each Mac; the relay unregistration
    /// queued by the notification cleanup covers a Mac that does not answer in time.
    private func removePushBindings() async {
        guard let storedRegistry else { return }
        let apis = storedRegistry.snapshot.entries.map(\.session.api)
        guard !apis.isEmpty else { return }
        let (settled, settle) = AsyncStream<Void>.makeStream()
        let removal = Task {
            await withTaskGroup(of: Void.self) { group in
                for api in apis { group.addTask { try? await api.removeServicePushBinding() } }
            }
            settle.yield()
        }
        let deadline = Task {
            try? await Task.sleep(for: .seconds(3))
            settle.yield()
        }
        for await _ in settled { break }
        removal.cancel()
        deadline.cancel()
    }
    private struct CleanupFailure: LocalizedError {
        let details: String
        var errorDescription: String? { details }
    }
}
