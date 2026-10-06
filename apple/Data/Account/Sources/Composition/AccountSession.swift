import AlondraNetworkReachability
import AlondraTransport
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
    private let pushBinding: @MainActor (ApprovedServicePairing) -> String?
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
    #if os(macOS)
    @ObservationIgnored private let directPairing = DirectPairingClient(devicePrefix: "client")
    #else
    @ObservationIgnored private let directPairing = DirectPairingClient(devicePrefix: "phone")
    #endif
    @ObservationIgnored private var discovered: [String: DiscoveredMac] = [:]
    @ObservationIgnored private var directAttempt: UUID?
    @ObservationIgnored private var discovery: Task<Void, Never>?
    @ObservationIgnored private var offeredServices: Set<String> = []
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
        $0.findMac = self.findMacClient
        $0.cloudPairing.classify = {
            ConnectionClassifier.problem(for: $0, local: false, networkOffline: NetworkReachability.shared.isOffline)
        }
        $0.cloudPairing.describe = { error in
            if let error = error as? PairingError, let message = error.errorDescription { return message }
            if let error = error as? AuthError, let message = error.errorDescription { return message }
            let problem = ConnectionClassifier.problem(for: error, local: false, networkOffline: NetworkReachability.shared.isOffline)
            return String(localized: problem.title)
        }
        $0.cloudPairing.hasSavedConfiguration = { [self] in
            if try await auth.requiresRevocation() { return true }
            return try await pairing.approvedPairing() != nil
        }
    }
    public init(auth: AuthSession, displayName: String, storageDirectory: URL,
                prepareCamera: @escaping @Sendable () async -> Bool = { false },
                didReconcile: @escaping @MainActor (ApprovedSessionRegistry<Connection>.Snapshot) async throws -> (@MainActor () throws -> Void) = { _ in {} },
                willDeactivate: @escaping @MainActor () async throws -> Void = {},
                pushBinding: @escaping @MainActor (ApprovedServicePairing) -> String? = { _ in nil }) {
        self.auth = auth
        self.pushBinding = pushBinding
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

    /// Pairing with a Mac on this network, for the first screen and for Settings' Add a Mac.
    public nonisolated var findMacClient: FindMacClient {
        FindMacClient(browse: { [self] in
            AsyncStream { continuation in
                let task = Task { @MainActor in
                    for await macs in MacBrowser.browse() { continuation.yield(self.found(macs)) }
                    continuation.finish()
                }
                continuation.onTermination = { _ in task.cancel() }
            }
        }, pair: { [self] target in try await pairDirect(target) },
        approve: { [self] in try await approveDirect() },
        cancel: { [self] in await cancelDirect() },
        describe: { error in
            if error is CancellationError { return String(localized: "Pairing was cancelled.") }
            // The other pairing errors speak about the QR code, which this path never shows.
            if let error = error as? PairingError,
               [.alreadyLinked, .denied, .busy, .macUnreachable, .requestExpired].contains(error),
               let sentence = error.errorDescription { return sentence }
            return String(localized: "Alondra couldn’t pair with that Mac. Try again.")
        })
    }

    private func found(_ macs: [DiscoveredMac]) -> [FoundMac] {
        for mac in macs { discovered[mac.id] = mac }
        let paired = Set(registry.snapshot.entries.map(\.approved.macID))
        return macs.map { FoundMac(id: $0.id, name: $0.name, paired: $0.macID.map(paired.contains) ?? false) }
    }

    private func pairDirect(_ target: FindMacTarget) async throws -> DirectPairingCode {
        await cancelDirect()
        let endpoint: DirectEndpoint
        switch target {
        case .found(let id):
            guard let mac = discovered[id] else { throw PairingError.macUnreachable }
            endpoint = try await MacBrowser.resolve(mac)
        case .address(let text):
            guard let parsed = Self.endpoint(address: text) else { throw PairingError.macUnreachable }
            endpoint = parsed
        }
        let attempt = try await directPairing.begin(at: endpoint, displayName: displayName)
        directAttempt = attempt.id
        return DirectPairingCode(code: attempt.code, macName: attempt.macName)
    }

    private func approveDirect() async throws {
        guard let id = directAttempt else { throw PairingError.missingAttempt }
        let current = generation
        let approved = try await directPairing.waitForApproval(id)
        if directAttempt == id { directAttempt = nil }
        guard current == generation else { throw CancellationError() }
        let collection = try await pairing.approvedPairings()
        try await reconcile(collection, subject: registry.ownerSubject, preferred: ApprovedPairingID(approved), generation: current)
    }

    private func cancelDirect() async {
        guard let directAttempt else { return }
        self.directAttempt = nil
        await directPairing.discard(directAttempt)
    }

    /// "host" or "host:port" typed by the person; the direct port is 47810 unless they say otherwise.
    static func endpoint(address text: String) -> DirectEndpoint? {
        var host = text.trimmingCharacters(in: .whitespaces)
        var port: UInt16 = 47810
        if host.hasPrefix("["), let close = host.firstIndex(of: "]") {
            let rest = host[host.index(after: close)...]
            if rest.hasPrefix(":") { guard let parsed = UInt16(rest.dropFirst()) else { return nil }; port = parsed }
            else if !rest.isEmpty { return nil }
            host = String(host[host.index(after: host.startIndex)..<close])
        } else if host.filter({ $0 == ":" }).count == 1, let colon = host.lastIndex(of: ":") {
            guard let parsed = UInt16(host[host.index(after: colon)...]) else { return nil }
            port = parsed
            host = String(host[..<colon])
        }
        let octets = host.split(separator: ".").compactMap { UInt8($0) }
        let tailnet = octets.count == 4 && octets[0] == 100 && (64..<128).contains(octets[1])
            || host.lowercased().hasSuffix(".ts.net")
        let endpoint = DirectEndpoint(host: host, port: port, kind: tailnet ? .tailscale : .localNetwork)
        return host.isEmpty || port == 0 ? nil : endpoint
    }

    /// Forgets a Mac paired on the local network. Its keys leave this device; the Mac keeps
    /// listing the device until someone revokes it there.
    public func forgetMac(choice id: UUID) async throws {
        guard let pairingID = choiceIDs.first(where: { $0.value == id })?.key, pairingID.ownerSub == nil else {
            throw CancellationError()
        }
        let current = generation
        try await pairing.removePairing(pairingID)
        let collection = try await pairing.approvedPairings()
        try await reconcile(collection, subject: registry.ownerSubject, preferred: collection.selectedID, generation: current)
    }

    /// How every paired Mac answers now: one signed round trip each, all at once.
    public func macStatuses() async -> [UUID: MacStatus] {
        let entries = registry.snapshot.entries.compactMap { entry in choiceIDs[entry.id].map { ($0, entry.session) } }
        return await withTaskGroup(of: (UUID, MacStatus).self) { group in
            for (id, session) in entries {
                group.addTask {
                    do {
                        try await session.checkSettingsConnection()
                        return (id, await session.route().map(MacStatus.route) ?? .problem(.unexpected))
                    } catch { return (id, .problem(session.connectionProblem(for: error))) }
                }
            }
            var statuses: [UUID: MacStatus] = [:]
            for await (id, status) in group { statuses[id] = status }
            return statuses
        }
    }

    /// Every paired Mac says where it answers directly, and Bonjour adds what this network sees.
    /// A relay pairing gains the direct path this way, without pairing again.
    private func learnRoutes(generation current: Int) {
        for entry in registry.snapshot.entries {
            let id = entry.id, session = entry.session, pairing = pairing
            Task { [weak self] in
                guard let endpoints = try? await session.learnRoutes(), self?.generation == current else { return }
                try? await pairing.updateRoutes(id, routes: endpoints, name: nil)
            }
        }
        guard discovery == nil, !registry.snapshot.entries.isEmpty else { return }
        discovery = Task { [weak self] in
            for await macs in MacBrowser.browse() {
                guard let self else { return }
                await self.offerDiscovered(macs)
            }
        }
    }

    private func offerDiscovered(_ macs: [DiscoveredMac]) async {
        for mac in macs {
            guard let macID = mac.macID, !offeredServices.contains(mac.id + macID),
                  let entry = registry.snapshot.entries.first(where: { $0.approved.macID == macID }) else { continue }
            offeredServices.insert(mac.id + macID)
            guard let endpoint = try? await MacBrowser.resolve(mac) else { offeredServices.remove(mac.id + macID); continue }
            await entry.session.offer([endpoint])
            try? await pairing.updateRoutes(entry.id, routes: [endpoint], name: nil)
        }
    }

    /// A QR may name the relay but cannot authorize a destination for the account bearer.
    private static func validateRelay(_ url: URL?) throws {
        guard let url, url.scheme == "https", url.host()?.lowercased() == "alondra-api.davidcollado.dev",
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
    /// Macs paired on the local network come back without an account. The account is checked
    /// only for relay pairings, and its failure stops the restore only when nothing else is saved.
    private func restoreApprovedPairing() async throws -> Bool {
        let current = generation
        let collection = try await pairing.approvedPairings()
        let hasLocal = collection.pairings.contains { !$0.hasRelay }
        let subject: String?
        do { subject = try await relaySubject(collection) }
        catch where hasLocal && !(error is CancellationError) { subject = nil }
        guard !collection.pairings.isEmpty else { return false }
        try Task.checkCancellation()
        guard current == generation else { throw CancellationError() }
        try await reconcile(collection, subject: subject, preferred: collection.selectedID, generation: current)
        return true
    }

    private func relaySubject(_ collection: ApprovedPairingCollection) async throws -> String? {
        let relayPairings = collection.pairings.filter(\.hasRelay)
        guard collection.pairings.isEmpty || !relayPairings.isEmpty else { return nil }
        do { _ = try await auth.hasSession() }
        catch AuthError.pendingRevocation, AuthError.rotationUncertain {
            throw RestoreError.revocationRequired
        }
        guard !relayPairings.isEmpty else { return nil }
        let subject: String
        do { subject = try await auth.verifiedSubject() }
        catch AuthError.noSession { throw RestoreError.reauthenticationRequired }
        let authorized = relayPairings.filter { $0.ownerSub == subject }
        guard !authorized.isEmpty else {
            throw RestoreError.reauthenticationRequired
        }
        for approved in authorized { try Self.validateRelay(approved.relayURL) }
        return subject
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
        var subject: String?
        if id.ownerSub != nil {
            subject = try await auth.verifiedSubject()
            guard current == generation, id.ownerSub == subject else { throw CancellationError() }
            try Self.validateRelay(id.relayURL)
        }
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

    private func reconcile(_ collection: ApprovedPairingCollection, subject: String?,
                           preferred: ApprovedPairingID?, generation current: Int) async throws {
        let authorized = collection.pairings.filter { !$0.hasRelay || $0.ownerSub == subject }
        for approved in authorized where approved.hasRelay { try Self.validateRelay(approved.relayURL) }
        guard current == generation, !Task.isCancelled else { throw CancellationError() }
        try await registry.reconcile(authorized, verifiedSubject: subject, preferred: preferred)
        guard current == generation, !Task.isCancelled else { throw CancellationError() }
        publishChoices()
        learnRoutes(generation: current)
    }

    private func publishChoices() {
        let snapshot = registry.snapshot
        choiceIDs = Dictionary(uniqueKeysWithValues: snapshot.entries.map { ($0.id, choiceIDs[$0.id] ?? UUID()) })
        macs = snapshot.entries.compactMap { entry in
            choiceIDs[entry.id].map {
                MacChoice(id: $0, macID: entry.id.macID, name: entry.approved.name, local: !entry.approved.hasRelay)
            }
        }.sorted { ($0.name ?? $0.macID, $0.macID) < ($1.name ?? $1.macID, $1.macID) }
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
        discovery?.cancel(); discovery = nil; offeredServices = []
        await cancelDirect()
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
            try await pairing.removeRelayPairings()
        } catch { failures.append(error.localizedDescription) }
        if !failures.isEmpty { throw CleanupFailure(details: failures.joined(separator: "\n")) }
        // Macs paired on the local network never depended on the account.
        _ = try? await restore()
    }
    /// Best effort while the sessions can still reach each Mac; the relay unregistration
    /// queued by the notification cleanup covers a Mac that does not answer in time.
    private func removePushBindings() async {
        guard let storedRegistry else { return }
        let removals = storedRegistry.snapshot.entries.compactMap { entry in
            pushBinding(entry.approved).map { (entry.session.api, $0) }
        }
        guard !removals.isEmpty else { return }
        let (settled, settle) = AsyncStream<Void>.makeStream()
        let removal = Task {
            await withTaskGroup(of: Void.self) { group in
                for (api, binding) in removals { group.addTask { try? await api.removeServicePushBinding(binding) } }
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
