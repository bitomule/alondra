import AlondraAPIClient
import AlondraCommands
import AlondraDeviceIdentity
import AlondraiOSFeature
import AlondraShared
import AlondraSync
import AlondraTriageFeature
import ComposableArchitecture
import Foundation
import SwiftUI

/// Process lifetime transport owner. Scenes subscribe independently to one SyncSession.
@MainActor
final class IOSAppSync {
    let session: SyncSession
    private let transport: any HTTPTransport
    private let fleet: FleetStore
    private let pairing: DevicePairing
    private let push: PushCenter
    private var identity: IOSSyncIdentity
    private var isConfigurationSuspended: Bool
    private var commandGeneration: UInt64 = 1
    private var configurationEpoch: UInt64 = 0
    private var foregroundScenes: Set<UUID> = []
    private var activityTask: Task<Void, Never>?
    private var activityGeneration: UInt64 = 0
    private var lastProjection: (state: SyncState, update: IOSRootUpdate)?
    private var triageScenes: [UUID: StoreOf<TriageFeature>] = [:]
    private var triageObservers: [UUID: AsyncStream<TriageInboxUpdate>.Continuation] = [:]
    private var lastTriageUpdate: TriageInboxUpdate?

    private var triageIdentity: TriageIdentity {
        TriageIdentity(server: identity.server, deviceID: identity.deviceID,
            credentialRevision: identity.credentialRevision, generation: commandGeneration)
    }

    func attachTriage(_ store: StoreOf<TriageFeature>, sceneID: UUID) {
        triageScenes[sceneID] = store
        store.send(.identityChanged(triageIdentity))
        store.send(.appeared)
    }

    func detachTriage(sceneID: UUID) {
        if let store = triageScenes.removeValue(forKey: sceneID) { store.send(.disappeared) }
    }

    func observeTriage() -> AsyncStream<TriageInboxUpdate> {
        let id = UUID()
        let (stream, continuation) = AsyncStream<TriageInboxUpdate>.makeStream(bufferingPolicy: .bufferingNewest(1))
        triageObservers[id] = continuation
        if let lastTriageUpdate, lastTriageUpdate.identity == triageIdentity {
            continuation.yield(lastTriageUpdate)
        }
        continuation.onTermination = { [weak self] _ in
            Task { @MainActor [weak self] in _ = self?.triageObservers.removeValue(forKey: id) }
        }
        return stream
    }

    private func resetTriageIdentity() {
        lastTriageUpdate = nil
        for store in triageScenes.values { store.send(.identityChanged(triageIdentity)) }
    }

    func mutateTriage(_ kind: CommandKind, itemID: String, expected: TriageIdentity) async -> TriageMutationOutcome {
        guard expected == triageIdentity else { return .obsolete }
        let client = fleet.client
        let result = await fleet.submitCommand(kind, itemID: itemID,
            expectedCredentialRevision: expected.credentialRevision, using: client) { client in
            switch kind {
            case .markRead(let id): try await client.markRead(id)
            case .dismissPullRequest(let id): try await client.dismissPullRequest(id)
            default: throw TriageAdapterError.unsupported
            }
        }
        guard expected == triageIdentity else { return .obsolete }
        switch result {
        case .confirmed: return .confirmed
        case .unknown: return .unknown
        case .obsolete: return .obsolete
        case .rejected(let error): return .rejected(error.localizedDescription)
        }
    }

    private enum TriageAdapterError: Error { case unsupported }

    init(fleet: FleetStore, pairing: DevicePairing, push: PushCenter) {
        self.fleet = fleet
        self.pairing = pairing
        self.push = push
        let transport = URLSessionTransport()
        self.transport = transport
        let address = UserDefaults.standard.string(forKey: ServerSetting.key) ?? ServerSetting.defaultURL
        let configuredURL = AlondraClient.serverURL(address)
        let base = configuredURL ?? AlondraClient.defaultURL
        let client = AlondraClient(base: base, device: "iphone", signer: pairing.signer(for: base))
        let identity = Self.identity(for: client)
        self.identity = identity
        self.isConfigurationSuspended = configuredURL == nil
        fleet.use(client, commandScope: Self.commandScope(for: identity, generation: 1))
        session = SyncSession(
            client: AlondraAPIClient(base: client.base, device: client.device, signer: client.signer, transport: transport),
            credentialRevision: identity.credentialRevision,
            initialInvalidConfiguration: configuredURL == nil ? String(localized: "Invalid server URL") : nil
        )
        if configuredURL == nil { fleet.suspendForInvalidConfiguration(String(localized: "Invalid server URL")) }
        Task { await fleet.restoreUncertainSends() }
    }

    static func identity(for client: AlondraClient) -> IOSSyncIdentity {
        let revision = (client.signer as? DeviceSigner)?.key.publicKey.x963Representation.base64EncodedString() ?? "unpaired"
        return IOSSyncIdentity(
            server: client.base,
            deviceID: client.signer?.deviceID ?? client.device,
            credentialRevision: revision
        )
    }

    static func commandScope(for identity: IOSSyncIdentity, generation: UInt64) -> CommandScope {
        CommandScope(
            server: identity.server,
            deviceID: identity.deviceID,
            credentialRevision: identity.credentialRevision,
            generation: generation
        )
    }

    func use(serverAddress: String) async {
        let configuredAddress = UserDefaults.standard.string(forKey: ServerSetting.key) ?? ServerSetting.defaultURL
        guard configuredAddress == serverAddress else { return }
        guard let url = AlondraClient.serverURL(serverAddress) else {
            configurationEpoch &+= 1
            commandGeneration &+= 1
            resetTriageIdentity()
            let reason = String(localized: "Invalid server URL")
            activityGeneration &+= 1
            activityTask?.cancel()
            activityTask = nil
            isConfigurationSuspended = true
            fleet.suspendForInvalidConfiguration(reason)
            await session.suspendForInvalidConfiguration(reason)
            return
        }
        let client = AlondraClient(base: url, device: "iphone", signer: pairing.signer(for: url))
        let next = Self.identity(for: client)
        guard next != identity || isConfigurationSuspended else { return }
        configurationEpoch &+= 1
        activityGeneration &+= 1
        activityTask?.cancel()
        activityTask = nil
        identity = next
        isConfigurationSuspended = false
        commandGeneration &+= 1
        resetTriageIdentity()
        fleet.use(client, commandScope: Self.commandScope(for: next, generation: commandGeneration))
        await fleet.restoreUncertainSends()
        await session.use(
            AlondraAPIClient(base: client.base, device: client.device, signer: client.signer, transport: transport),
            credentialRevision: next.credentialRevision
        )
        push.scheduleSync()
        startActivityIfNeeded()
    }

    /// Decode and project at the main actor boundary, only for the current identity.
    /// An old window event cannot publish a stale snapshot after a server switch.
    func project(_ value: SyncState) async -> IOSRootUpdate? {
        let epoch = configurationEpoch
        let current = await session.state
        guard epoch == configurationEpoch else { return nil }
        guard value.generation == current.generation,
              value.identity == current.identity,
              value.publicationRevision == current.publicationRevision else { return nil }
        let eventIdentity = IOSSyncIdentity(
            server: value.identity.server,
            deviceID: value.identity.deviceID,
            credentialRevision: value.identity.credentialRevision
        )
        guard eventIdentity == identity else { return nil }
        if isConfigurationSuspended {
            guard case .invalidConfiguration = value.connection else { return nil }
        } else if case .invalidConfiguration = value.connection {
            return nil
        }
        // A revision belongs to one SyncSession publication, not one scene. Reusing that
        // publication across windows is safe; an identical later overview must project again.
        if let lastProjection, lastProjection.state == value {
            let cached = lastProjection.update
            return IOSRootUpdate(
                identity: cached.identity,
                generation: cached.generation,
                connection: cached.connection,
                hasSnapshot: cached.hasSnapshot,
                inboxCount: cached.hasSnapshot ? fleet.queue.count : cached.inboxCount,
                resetsPresentation: cached.resetsPresentation
            )
        }
        let overview: Overview?
        var invalidSnapshot = false
        if let snapshot = value.snapshot {
            let decoder = JSONDecoder()
            decoder.keyDecodingStrategy = .convertFromSnakeCase
            do { overview = try decoder.decode(Overview.self, from: snapshot.body) }
            catch {
                overview = nil
                invalidSnapshot = true
            }
        } else {
            overview = nil
        }
        let connection: IOSRootFeature.Connection
        let legacyConnection: FleetStore.Connection
        switch value.connection {
        case .connecting:
            connection = .connecting
            legacyConnection = .connecting
        case .online:
            if invalidSnapshot {
                let reason = String(localized: "Invalid server overview")
                connection = .offline(reason)
                legacyConnection = .offline(reason)
            } else {
                connection = .online
                legacyConnection = .online
            }
        case .offline(let reason):
            connection = .offline(reason)
            legacyConnection = .offline(reason)
        case .invalidConfiguration(let reason):
            connection = .offline(reason)
            legacyConnection = .offline(reason)
        case .unpaired:
            connection = .unpaired
            legacyConnection = .unpaired
        }
        if let overview { fleet.apply(overview) }
        fleet.setConnection(legacyConnection)
        let triageSnapshot: Overview?
        if case .online = value.connection { triageSnapshot = overview }
        else { triageSnapshot = nil }
        let triageUpdate = TriageInboxUpdate(identity: triageIdentity, overview: triageSnapshot)
        lastTriageUpdate = triageUpdate
        for observer in triageObservers.values { observer.yield(triageUpdate) }
        if overview != nil { push.inboxChanged(pending: fleet.queue) }
        let update = IOSRootUpdate(
            identity: eventIdentity,
            generation: value.generation,
            connection: connection,
            hasSnapshot: overview != nil,
            inboxCount: {
                if case .invalidConfiguration = value.connection { return 0 }
                return overview == nil ? nil : fleet.queue.count
            }(),
            resetsPresentation: {
                if case .invalidConfiguration = value.connection { return true }
                return false
            }()
        )
        lastProjection = (value, update)
        return update
    }

    func activatePushIfPaired() async {
        guard pairing.isPaired else { return }
        await push.activate()
        if !isConfigurationSuspended { push.scheduleSync() }
        startActivityIfNeeded()
    }

    func refreshPushAuthorization() async {
        await push.refreshAuthorization()
    }

    func setForegroundActivity(sceneID: UUID, active: Bool) {
        if active { foregroundScenes.insert(sceneID) }
        else { foregroundScenes.remove(sceneID) }
        if foregroundScenes.isEmpty {
            activityGeneration &+= 1
            activityTask?.cancel()
            activityTask = nil
        } else {
            startActivityIfNeeded()
        }
    }

    private func startActivityIfNeeded() {
        guard activityTask == nil, !foregroundScenes.isEmpty, pairing.isPaired, !isConfigurationSuspended else { return }
        activityGeneration &+= 1
        let generation = activityGeneration
        activityTask = Task { @MainActor [weak self] in
            guard let self else { return }
            defer { if activityGeneration == generation { activityTask = nil } }
            while !Task.isCancelled && !foregroundScenes.isEmpty && pairing.isPaired {
                try? await fleet.client.activity(client: "iphone")
                do { try await Task.sleep(for: .seconds(60)) } catch { return }
            }
        }
    }
}

/// Scene-owned TCA state; shared clients stay outside SwiftUI View value types.
@MainActor
final class IOSWindowContext {
    let feature: StoreOf<IOSRootFeature>
    let triage: StoreOf<TriageFeature>

    init(services: IOSAppSync) {
        triage = Store(initialState: TriageFeature.State()) {
            TriageFeature()
        } withDependencies: {
            $0.triageService = TriageService(
                observe: { [services] in await services.observeTriage() },
                mutate: { [services] kind, itemID, identity in
                    await services.mutateTriage(kind, itemID: itemID, expected: identity)
                }
            )
        }
        feature = Store(initialState: IOSRootFeature.State()) {
            IOSRootFeature()
        } withDependencies: {
            $0.iosRootSync = .live(session: services.session)
            $0.iosRootProjection = IOSRootProjection(project: { [services] value in
                await services.project(value)
            })
            $0.iosRootLifecycle = IOSRootLifecycle(
                configure: { [services] address in await services.use(serverAddress: address) },
                activatePush: { [services] in await services.activatePushIfPaired() },
                refreshAuthorization: { [services] in await services.refreshPushAuthorization() },
                setForegroundActivity: { [services] id, active in await services.setForegroundActivity(sceneID: id, active: active) }
            )
        }
    }
}
