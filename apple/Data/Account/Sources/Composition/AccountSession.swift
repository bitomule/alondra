import AlondraContract
import AlondraNetworkReachability
import AlondraTransport
import AlondraPairing
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import Observation
import Foundation

/// Process-owned credential and pairing use cases. Secrets never enter presentation state.
@MainActor
@Observable
public final class AccountSession {
    public let auth: AuthSession
    /// Every request this app makes to its Macs, kept to compare connection paths.
    public let measurements: ConnectionMeasurements
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
        let storageDirectory = storageDirectory
        let measurements = measurements
        let irohDialer = irohDialer
        let localNetwork = localNetwork
        let accountTransport = accountTransport
        return ApprovedSessionRegistry<Connection>(
            make: { [weak self] approved in
                let connection = try await Connection(approved: approved, auth: auth, storageDirectory: storageDirectory,
                                                      measurements: measurements,
                                                      localNetworkAllowed: { localNetwork.allowed }, irohDialer: irohDialer, accountTransport: accountTransport)
                if await MainActor.run(body: { self?.isLive == true }) { await connection.goLive() }
                return connection
            },
            stop: { await $0.deactivate() }, publish: { [weak self] snapshot in
                guard let self else { throw CancellationError() }
                return try await self.didReconcile(snapshot)
            }, clearPlatform: { [weak self] in try await self?.willDeactivate() })
    }
    private let pairing: PairingClient
    private let accountTransport: (any HTTPTransport)?
    @ObservationIgnored private let irohDialer: (@Sendable (IrohIdentity) -> any IrohDialer)?
    /// Whether the app is in the foreground, where every Mac keeps its live connection open.
    @ObservationIgnored private var isLive = false
    @ObservationIgnored private let directPairing: DirectPairingClient
    @ObservationIgnored private var discovered: [String: DiscoveredMac] = [:]
    @ObservationIgnored private var directAttempt: UUID?
    @ObservationIgnored private var discovery: Task<Void, Never>?
    @ObservationIgnored private var keepingRoutes: Task<Void, Never>?
    @ObservationIgnored private var routeLearning: [UUID: Task<Void, Never>] = [:]
    @ObservationIgnored private var accountCheck: Task<Void, Never>?
    @ObservationIgnored private let browse: @Sendable () -> AsyncStream<MacBrowser.Update>
    private nonisolated let localNetwork: LocalNetworkAccess
    /// This Mac's own ID in the Mac app, so Find a Mac never offers the Mac it runs on.
    @ObservationIgnored public var ownMacID: String?
    /// The Mac the person chose to pair again: it is offered as pairable even though it is saved,
    /// and the saved pairing is replaced only when the new one is approved.
    @ObservationIgnored public private(set) var pairAgainMacID: String?
    /// Set by each app to the system screen where Local Network access is turned back on.
    @ObservationIgnored public var openLocalNetworkSettings: @MainActor () -> Void = {}
    /// Set by each app that offers alerts once a Mac is linked through the account.
    @ObservationIgnored public var alerts = SignInClient.Alerts.unavailable
    /// Set by each app: whether it is in front, with no system alert over it.
    @ObservationIgnored public var isActive: @MainActor () -> Bool = { true }
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
        $0.cloudPairing.alerts = SignInClient.Alerts(permission: { [self] in await alerts.permission() },
            request: { [self] in await alerts.request() }, openSettings: { [self] in await alerts.openSettings() })
        $0.findMac = self.findMacClient
        $0.cloudPairing.keepsLocalMacs = { [self] in (try? await restore()) ?? false }
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
                pushBinding: @escaping @MainActor (ApprovedServicePairing) -> String? = { _ in nil },
                pairingStore: any ApprovedPairingStore = KeychainApprovedPairingStore(),
                pairingTransport: (any HTTPTransport)? = nil,
                localNetwork: LocalNetworkAccess = .standard,
                browse: (@Sendable () -> AsyncStream<MacBrowser.Update>)? = nil,
                irohDialer: (@Sendable (IrohIdentity) -> any IrohDialer)? = nil, startsLive: Bool = false) {
        self.auth = auth
        self.accountTransport = pairingTransport
        isLive = startsLive
        self.irohDialer = irohDialer
        self.pushBinding = pushBinding
        self.localNetwork = localNetwork
        self.browse = browse ?? { MacBrowser.browse(access: localNetwork) }
        pairing = PairingClient(transport: pairingTransport, store: pairingStore)
        #if os(macOS)
        directPairing = DirectPairingClient(store: pairingStore, devicePrefix: "client", localNetwork: localNetwork)
        #else
        directPairing = DirectPairingClient(store: pairingStore, devicePrefix: "phone", localNetwork: localNetwork)
        #endif
        self.displayName = displayName
        self.storageDirectory = storageDirectory
        measurements = ConnectionMeasurements(file: storageDirectory.appending(path: "connection-measurements.jsonl"))
        self.prepareCamera = prepareCamera
        self.didReconcile = didReconcile
        self.willDeactivate = willDeactivate
    }
    public var additionalMacPairing: SignInClient {
        let scope = UUID()
        let expectedSession = services
        var service = SignInClient(restore: { throw CancellationError() }, login: { [self] in try await signInBesideLocalMacs() },
            begin: { [self] payload in
                guard let expectedSession else { throw CancellationError() }
                return try await begin(payload, scope: scope, expectedSession: expectedSession)
            }, approve: { [self] in
                guard let expectedSession else { throw CancellationError() }
                try await approve(scope: scope, expectedSession: expectedSession)
            },
            cancel: { [self] in
                await endPairAgain()
                await cancel(scope: scope)
            }, logout: { throw CancellationError() })
        service.prepareCamera = prepareCamera
        service.cancelAttempt = { [self] in await cancel(scope: scope) }
        return service
    }

    /// The next pairing on this network may replace the saved pairing of this Mac.
    public func pairAgain(macID: String?) { pairAgainMacID = macID }

    /// Pair Again on a Mac's connection screen, for a Mac paired on the network or through the
    /// relay: a device removed through the relay never gets the Mac's sealed revocation, so this
    /// is its only way back. `open` shows Link a Mac. None for the Mac this app runs on.
    public func pairAgainAction(for services: Connection, open: @escaping @MainActor () -> Void) -> (@MainActor () -> Void)? {
        guard let macID = services.macID else { return nil }
        return { [weak self] in
            self?.pairAgain(macID: macID)
            open()
        }
    }

    private func endPairAgain() { pairAgainMacID = nil }

    /// Pairing with a Mac on this network, for the first screen and for Settings' Add a Mac.
    public nonisolated var findMacClient: FindMacClient {
        FindMacClient(permission: { [localNetwork] in
            switch localNetwork.state {
            case .unknown: .unknown
            case .allowed: .allowed
            case .denied: .denied
            }
        }, browse: { [self] in
            AsyncStream { continuation in
                let task = Task { @MainActor in
                    await self.follow(continuation)
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
               [.alreadyLinked, .denied, .busy, .macUnreachable, .requestExpired, .thisMac, .insecureAddress].contains(error),
               let sentence = error.errorDescription { return sentence }
            return String(localized: "Alondra couldn’t pair with that Mac. Try again.")
        }, openSettings: { [self] in Task { @MainActor in self.openLocalNetworkSettings() } })
    }

    /// While the system's Local Network alert is on screen the browser already reads as refused,
    /// so a refusal counts only with the app in front; otherwise the browse starts over once the
    /// alert is gone, and the person's real answer is what gets saved.
    private func follow(_ continuation: AsyncStream<FindMacUpdate>.Continuation) async {
        while !Task.isCancelled {
            var askedWhileAway = false
            for await update in browse() {
                switch update {
                case .found(let macs): continuation.yield(.macs(found(macs)))
                case .allowed:
                    localNetwork.record(.allowed)
                    continuation.yield(.allowed)
                case .denied where isActive():
                    localNetwork.record(.denied)
                    continuation.yield(.denied)
                case .denied:
                    askedWhileAway = true
                }
                if askedWhileAway { break }
            }
            guard askedWhileAway else { return }
            while !isActive(), !Task.isCancelled { try? await Task.sleep(for: .milliseconds(300)) }
        }
    }

    private func found(_ macs: [DiscoveredMac]) -> [FoundMac] {
        let own = ownMacID
        let shown = macs.filter { $0.macID == nil || $0.macID != own }
        for mac in shown { discovered[mac.id] = mac }
        let paired = Set(registry.snapshot.entries.map(\.approved.macID)).subtracting(pairAgainMacID.map { [$0] } ?? [])
        return shown.map { FoundMac(id: $0.id, name: $0.name, paired: $0.macID.map(paired.contains) ?? false) }
    }

    private func pairDirect(_ target: FindMacTarget) async throws -> DirectPairingCode {
        await cancelDirect()
        let endpoint: DirectEndpoint
        switch target {
        case .found(let id):
            guard let mac = discovered[id] else { throw PairingError.macUnreachable }
            endpoint = try await MacBrowser.resolve(mac)
        case .address(let text):
            guard let parsed = DirectEndpoint(typed: text) else { throw PairingError.macUnreachable }
            endpoint = parsed
        }
        let attempt = try await directPairing.begin(at: endpoint, displayName: displayName, excluding: ownMacID,
                                                          replacing: pairAgainMacID)
        directAttempt = attempt.id
        return DirectPairingCode(code: attempt.code, macName: attempt.macName)
    }

    private func approveDirect() async throws {
        guard let id = directAttempt else { throw PairingError.missingAttempt }
        let current = generation
        let approved = try await directPairing.waitForApproval(id)
        if directAttempt == id { directAttempt = nil }
        pairAgainMacID = nil
        guard current == generation else { throw CancellationError() }
        let collection = try await pairing.approvedPairings()
        try await reconcile(collection, subject: registry.ownerSubject, preferred: ApprovedPairingID(approved), generation: current)
    }

    private func cancelDirect() async {
        guard let directAttempt else { return }
        self.directAttempt = nil
        await directPairing.discard(directAttempt)
    }

    /// Forgets a paired Mac, whether it was paired on the local network or through the account. Its
    /// keys leave this device; the Mac keeps listing the device until someone revokes it there.
    public func forgetMac(choice id: UUID) async throws {
        let ids = Set(choiceIDs.filter { $0.value == id }.map(\.key))
        guard !ids.isEmpty else { throw CancellationError() }
        let current = generation
        try await pairing.removePairings(ids)
        let collection = try await pairing.approvedPairings()
        try await reconcile(collection, subject: registry.ownerSubject, preferred: collection.selectedID, generation: current)
    }

    private func entries(choice id: UUID) -> [ApprovedSessionRegistry<Connection>.Entry] {
        let selected = registry.snapshot.selectedID
        return registry.snapshot.entries.filter { choiceIDs[$0.id] == id }.sorted {
            if ($0.id == selected) != ($1.id == selected) { return $0.id == selected }
            if $0.approved.approvedAtMs != $1.approved.approvedAtMs { return $0.approved.approvedAtMs > $1.approved.approvedAtMs }
            return $0.id.deviceID < $1.id.deviceID
        }
    }

    public func session(choice id: UUID) -> Connection? { entries(choice: id).first?.session }

    public func accountName() async -> String? {
        let current = generation
        guard let approved = registry.snapshot.entries.first(where: { $0.approved.hasRelay })?.approved,
              let relay = approved.relayURL else { return nil }
        do {
            try Self.validateRelay(relay)
            let subject = try await auth.verifiedSubject()
            guard subject == approved.ownerSub else { return nil }
            let directory = try RelayAccountDirectory(base: relay, bearer: { [auth] in try await auth.bearerToken() },
                                                       transport: accountTransport)
            let name = try await directory.accountName()
            guard generation == current else { return nil }
            return name
        } catch { return nil }
    }

    public func macStatuses() async -> [UUID: MacStatus] {
        let current = generation
        let entries = registry.snapshot.entries.compactMap { entry in
            choiceIDs[entry.id].map { ($0, entry.approved.hasRelay, entry.session) }
        }
        let states = await withTaskGroup(of: (UUID, Bool, ConnectionState, Bool).self) { group in
            for (id, account, session) in entries {
                group.addTask {
                    try? await session.checkSettingsConnection()
                    let removed = await session.accountRemoved()
                    return (id, account, await session.monitor.state, removed)
                }
            }
            var values: [(UUID, Bool, ConnectionState, Bool)] = []
            for await value in group { values.append(value) }
            return values
        }
        guard current == generation else { return [:] }
        return Dictionary(grouping: states, by: { $0.0 }).mapValues { values in
            MacStatus.combining(values.map { (local: !$0.1, state: $0.2, removed: $0.3) })
        }
    }

    /// Every paired Mac says where it answers directly, and Bonjour adds what this network sees.
    /// A relay pairing gains the direct path this way, without pairing again.
    private func learnRoutes(generation current: Int) {
        for entry in registry.snapshot.entries {
            let session = entry.session, id = UUID()
            routeLearning[id] = Task { [weak self] in
                try? await session.learnRoutes()
                self?.routeLearning[id] = nil
            }
        }
        startDiscovery()
        guard keepingRoutes == nil, !registry.snapshot.entries.isEmpty else { return }
        keepingRoutes = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(30))
                await self?.keepProvenRoutes()
            }
        }
    }

    /// Bonjour runs only for someone who paired a Mac on this network and has not turned Local
    /// Network access off; an account-only device never touches the local network on its own.
    private func startDiscovery() {
        guard discovery == nil, localNetwork.state != .denied,
              registry.snapshot.entries.contains(where: { !$0.approved.hasRelay }) else { return }
        let browse = browse
        discovery = Task { [weak self] in
            for await update in browse() {
                guard let self, case let .found(macs) = update else { continue }
                await self.offerDiscovered(macs)
            }
        }
    }

    /// Anyone on the network can advertise a paired Mac's ID, so each Mac gets a few resolved
    /// addresses at most, and they stay hints until a sealed answer proves one.
    private func offerDiscovered(_ macs: [DiscoveredMac]) async {
        for mac in macs {
            guard let macID = mac.macID, !offeredServices.contains(mac.id + macID),
                  offeredServices.filter({ $0.hasSuffix(macID) }).count < 4,
                  let entry = registry.snapshot.entries.first(where: { $0.approved.macID == macID }) else { continue }
            offeredServices.insert(mac.id + macID)
            guard let endpoint = try? await MacBrowser.resolve(mac) else { offeredServices.remove(mac.id + macID); continue }
            await entry.session.offerDiscovered(endpoint)
        }
    }

    /// Saves only the addresses a sealed answer proved.
    /// The app came to the foreground: every paired Mac opens its live connection, not only the
    /// one on screen, so switching Macs never waits for one.
    public func goLive() async {
        isLive = true
        for entry in registry.snapshot.entries { await entry.session.goLive() }
    }

    /// The app went to the background: the live connections close; push is what reaches it now.
    public func goIdle() async {
        isLive = false
        for entry in registry.snapshot.entries { await entry.session.goIdle() }
    }

    private func keepProvenRoutes() async {
        for entry in registry.snapshot.entries {
            if let endpoint = entry.session.learnedIrohEndpoint, endpoint != entry.approved.irohEndpoint {
                try? await pairing.updateIrohEndpoint(entry.id, endpoint: endpoint)
            }
            let proven = await entry.session.provenEndpoints()
            guard !proven.isEmpty else { continue }
            try? await pairing.updateRoutes(entry.id, routes: proven, name: nil)
        }
    }

    /// A QR may name the relay but cannot authorize a destination for the account bearer.
    private static func validateRelay(_ url: URL?) throws {
        guard let url, url.scheme == "https", url.host()?.lowercased() == ServiceHosts.relayHost,
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
    /// Macs paired on the local network come back without an account. With some of those saved,
    /// a failed account check never drops the account's Macs: they come back on what was saved,
    /// their own requests say whether the session expired, and the check runs again until it
    /// answers. A sign-out that never finished is never swallowed.
    private func restoreApprovedPairing() async throws -> Bool {
        let current = generation
        let collection = try await pairing.approvedPairings()
        let hasLocal = collection.pairings.contains { !$0.hasRelay }
        let subject: String?
        do { subject = try await relaySubject(collection) }
        catch RestoreError.revocationRequired { throw RestoreError.revocationRequired }
        catch where hasLocal && !(error is CancellationError) {
            subject = Self.savedOwner(collection)
            verifyAccountLater(generation: current)
        }
        guard !collection.pairings.isEmpty else { return false }
        try Task.checkCancellation()
        guard current == generation else { throw CancellationError() }
        try await reconcile(collection, subject: subject, preferred: collection.selectedID, generation: current)
        return true
    }

    /// The account the saved relay pairings belong to, the selected one first.
    private static func savedOwner(_ collection: ApprovedPairingCollection) -> String? {
        collection.selected?.ownerSub ?? collection.pairings.lazy.compactMap(\.ownerSub).first
    }

    /// Asks the account again until it answers. The same account keeps everything as it is;
    /// another one replaces the account's Macs; a real refusal leaves them showing that the
    /// session expired, which is where the person signs in again.
    private func verifyAccountLater(generation started: Int) {
        accountCheck?.cancel()
        accountCheck = Task { [weak self] in
            var attempt = 0
            while !Task.isCancelled {
                let delay = ConnectionBackoff.standard.delay(attempt: attempt, random: Double.random(in: 0...1))
                try? await Task.sleep(for: delay)
                attempt += 1
                guard let self, self.generation == started else { return }
                let collection: ApprovedPairingCollection
                do { collection = try await self.pairing.approvedPairings() } catch { continue }
                do {
                    let verified = try await self.relaySubject(collection)
                    guard self.generation == started else { return }
                    if verified != self.registry.ownerSubject {
                        try? await self.reconcile(collection, subject: verified, preferred: collection.selectedID,
                                                  generation: started)
                    }
                    return
                } catch RestoreError.reauthenticationRequired {
                    return
                } catch RestoreError.revocationRequired {
                    return
                } catch {
                    continue
                }
            }
        }
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
    /// Signing in from Settings keeps the Macs paired on this network: the account only adds the
    /// relay, and linking a Mac through it replaces that Mac's local pairing.
    private func signInBesideLocalMacs() async throws {
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
            rejectExistingMac: scope != nil, replacing: pairAgainMacID)
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
        do { approved = try await pairing.waitForApproval(attempt); pairAgainMacID = nil }
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
        let permission = scope == nil ? nil : await alerts.permission()
        guard expectedSession == nil || services === expectedSession else { throw CancellationError() }
        try await reconcile(collection, subject: subject, preferred: ApprovedPairingID(approved), generation: current)
        // The new Mac's workspace replaces the one Settings was open in, so the first screen
        // offers alerts, in the same update that shows the new Mac.
        if let permission { store.send(.offerAlerts(permission)) }
    }
    private func cancel(scope: UUID? = nil) async {
        if let ticket = attempts.current(scope: scope) { attempts.release(ticket) }
        else if scope != nil || attempts.hasPending { return }
        generation += 1
        if let attempt { self.attempt = nil; await pairing.discard(attempt) }
    }
    public func selectMac(choice id: UUID, expectedSession: Connection) async throws {
        try await SessionBoundAction.run(expected: expectedSession, current: { self.services }) { expected in
            let candidates = self.entries(choice: id)
            guard !candidates.isEmpty else { throw CancellationError() }
            var failure: (any Error)?
            for entry in candidates {
                do { try await self.selectMac(entry.id, expectedSession: expected); return }
                catch is CancellationError { throw CancellationError() }
                catch { failure = error }
            }
            if let failure { throw failure }
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
        let groups = Dictionary(grouping: snapshot.entries, by: { $0.id.macID })
        var next: [ApprovedPairingID: UUID] = [:]
        macs = groups.map { macID, entries in
            let ordered = entries.sorted { $0.id.deviceID < $1.id.deviceID }
            let id = ordered.compactMap { choiceIDs[$0.id] }.first ?? UUID()
            for entry in entries { next[entry.id] = id }
            return MacChoice(id: id, macID: macID, name: ordered.compactMap { $0.approved.name }.first,
                             local: entries.allSatisfy { !$0.approved.hasRelay }, hasAlternatePairings: entries.count > 1)
        }.sorted { ($0.name ?? $0.macID, $0.macID) < ($1.name ?? $1.macID, $1.macID) }
        choiceIDs = next
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
        keepingRoutes?.cancel(); keepingRoutes = nil
        accountCheck?.cancel(); accountCheck = nil
        routeLearning.values.forEach { $0.cancel() }; routeLearning = [:]
        await cancelDirect()
        await pairing.discardAllAttempts()
        try await registry.clear()
    }

    /// Ends this session for good: every Mac disconnects and every task it started has
    /// finished, so nothing it runs writes to its storage directory afterwards.
    public func stop() async throws {
        let started = [discovery, keepingRoutes, accountCheck].compactMap { $0 } + routeLearning.values
        let restoring = restoreTask
        var failure: (any Error)?
        do { try await deactivate() } catch { failure = error }
        for task in started { await task.value }
        _ = try? await restoring?.value
        if let failure { throw failure }
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
