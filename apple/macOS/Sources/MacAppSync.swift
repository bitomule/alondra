import AlondraAPIClient
import AlondraCommands
import AlondraDeviceIdentity
import AlondraMacFeature
import AlondraShared
import AlondraSync
import AlondraTriageFeature
import AppKit
import ComposableArchitecture
import Foundation
import Observation
import SwiftUI

/// Process lifetime owner. Every window subscribes to this same SyncSession actor.
@MainActor
@Observable
final class MacAppSync {
    let session: SyncSession
    let commandGate: CommandGate
    private(set) var commandScope: CommandScope
    private var identity: MacSyncIdentity
    private var activatedGeneration: UInt64 = 0
    private let transport: any HTTPTransport
    private var pairingTask: Task<Void, Never>?
    private var activityTask: Task<Void, Never>?
    private var client: AlondraClient
    private var windows: [WeakWindowBridge] = []
    private(set) var pairingError: String?

    private final class WeakWindowBridge {
        weak var value: MacLegacyProjection?
        init(_ value: MacLegacyProjection) { self.value = value }
    }

    init(
        client: AlondraClient,
        commandJournal: (any CommandJournaling)? = FileCommandJournal(directory: CommandJournalLocation.applicationSupport(
            bundleID: Bundle.main.bundleIdentifier ?? "com.bitomule.alondra.mac"))
    ) {
        let identity = Self.identity(for: client)
        let transport = URLSessionTransport()
        self.identity = identity
        self.transport = transport
        self.client = client
        let scope = CommandScope(
            server: identity.server,
            deviceID: identity.deviceID,
            credentialRevision: identity.credentialRevision,
            generation: 0
        )
        commandScope = scope
        commandGate = CommandGate(scope: scope, journal: commandJournal)
        session = SyncSession(
            client: AlondraAPIClient(base: client.base, device: client.device, signer: client.signer, transport: transport),
            credentialRevision: identity.credentialRevision
        )
    }

    static func identity(for client: AlondraClient) -> MacSyncIdentity {
        // The public key changes when a signing key rotates, even if the server reuses the ID.
        let revision = (client.signer as? DeviceSigner)?.key.publicKey.x963Representation.base64EncodedString() ?? "unpaired"
        return MacSyncIdentity(
            server: client.base,
            deviceID: client.signer?.deviceID ?? client.device,
            credentialRevision: revision
        )
    }

    func register(_ bridge: MacLegacyProjection) {
        windows.removeAll { $0.value == nil }
        windows.append(WeakWindowBridge(bridge))
        Task { await bridge.fleet.restoreUncertainSends() }
    }

    /// The Mac's existing CommandGate owns triage mutations. No window creates a client or a poll.
    func mutateTriage(_ kind: CommandKind, itemID: String, identity: TriageIdentity) async -> TriageMutationOutcome {
        let scope = commandScope
        guard scope.server == identity.server,
              scope.deviceID == identity.deviceID,
              scope.credentialRevision == identity.credentialRevision,
              scope.generation == identity.generation else { return .obsolete }
        let client = self.client
        let result = await commandGate.submit(kind, itemID: itemID, in: scope,
            isUnknown: { $0.isTransient || $0.isCancellation }) {
            switch kind {
            case .markRead(let id): try await client.markRead(id)
            case .dismissPullRequest(let id): try await client.dismissPullRequest(id)
            default: throw MacTriageMutationError.unsupported
            }
        }
        guard commandScope == scope else { return .obsolete }
        switch result {
        case .confirmed: return .confirmed
        case .unknown: return .unknown
        case .obsolete: return .obsolete
        case .rejected(let error): return .rejected(error.localizedDescription)
        }
    }

    /// Capture the command scope at the user gesture, before Swift schedules an async task.
    func runMutation(
        onDiscard: @escaping @MainActor () -> Void = {},
        _ operation: @escaping @MainActor () async -> Void
    ) {
        let origin = commandScope
        Task { @MainActor in
            guard commandScope == origin else { onDiscard(); return }
            await operation()
        }
    }

    /// Synchronous invalidation makes every window drop the previous identity before an
    /// in-flight decode can return to the main actor. The async activation follows below.
    @discardableResult
    func prepareUse(_ client: AlondraClient) -> CommandScope {
        let next = Self.identity(for: client)
        guard next != identity else { return commandScope }
        identity = next
        self.client = client
        commandScope = CommandScope(
            server: next.server,
            deviceID: next.deviceID,
            credentialRevision: next.credentialRevision,
            generation: commandScope.generation &+ 1
        )
        windows.removeAll { $0.value == nil }
        for window in windows { window.value?.use(client, commandScope: commandScope) }
        return commandScope
    }

    func activate(_ client: AlondraClient, scope: CommandScope) async {
        guard scope.generation > activatedGeneration else { return }
        activatedGeneration = scope.generation
        await commandGate.use(scope)
        guard scope == commandScope else { return }
        for window in windows { await window.value?.fleet.restoreUncertainSends() }
        guard scope == commandScope else { return }
        await session.use(
            AlondraAPIClient(base: client.base, device: client.device, signer: client.signer, transport: transport),
            credentialRevision: scope.credentialRevision
        )
    }

    /// One activity heartbeat source for the process, independent of the window count.
    func startActivityMonitoring() {
        guard activityTask == nil else { return }
        activityTask = Task { @MainActor in
            let (events, continuation) = AsyncStream<Void>.makeStream(bufferingPolicy: .bufferingNewest(1))
            let monitor = NSEvent.addLocalMonitorForEvents(matching: [.keyDown, .leftMouseDown, .rightMouseDown, .scrollWheel]) { event in
                continuation.yield()
                return event
            }
            defer {
                if let monitor { NSEvent.removeMonitor(monitor) }
                continuation.finish()
                activityTask = nil
            }
            var told = Date.distantPast
            for await _ in events {
                guard !Task.isCancelled else { return }
                let now = Date()
                guard now.timeIntervalSince(told) >= 60 else { continue }
                told = now
                try? await client.activity(client: "mac")
            }
        }
    }

    /// Coalesces pairing attempts from multiple open windows.
    func pairIfNeeded(device: LocalDevice, connection: FleetStore.Connection) async {
        guard connection == .unpaired || (connection == .online && !device.isPaired) else { return }
        guard !device.isPaired else { return }
        if let pairingTask {
            await pairingTask.value
            return
        }
        let task = Task { @MainActor in
            defer { pairingTask = nil }
            while !Task.isCancelled && !device.isPaired {
                do {
                    try await device.pair()
                    pairingError = nil
                    return
                } catch where error.isCancellation {
                    return
                } catch {
                    pairingError = String(localized: "Couldn’t pair this Mac with alondrad: \(error.friendlyDescription)")
                    try? await Task.sleep(for: .seconds(30))
                }
            }
        }
        pairingTask = task
        await task.value
    }
}

/// One bridge per window until the feature migrations remove FleetStore altogether.
@MainActor
final class MacLegacyProjection {
    let fleet: FleetStore
    let drafts: Drafts
    weak var feature: StoreOf<MacWindowFeature>?
    weak var triage: StoreOf<TriageFeature>?
    private var gate: MacProjectionGate
    private var commandScope: CommandScope
    private var lastSnapshotBody: Data?

    init(client: AlondraClient, commandGate: CommandGate, commandScope: CommandScope, drafts: Drafts) {
        fleet = FleetStore(client: client, commandGate: commandGate, commandScope: commandScope)
        self.drafts = drafts
        gate = MacProjectionGate(expected: MacAppSync.identity(for: client))
        self.commandScope = commandScope
    }

    func use(_ client: AlondraClient, commandScope: CommandScope) {
        let next = MacAppSync.identity(for: client)
        let previous = gate.expected
        guard next != previous || commandScope != self.commandScope else { return }
        feature?.send(.identityWillChange(next))
        triage?.send(.identityChanged(Self.triageIdentity(commandScope)))
        gate.use(next)
        self.commandScope = commandScope
        drafts.discardAll()
        lastSnapshotBody = nil
        fleet.use(client, commandScope: commandScope)
    }

    func apply(_ state: SyncState, overview: Overview?) -> MacWindowUpdate? {
        guard !Task.isCancelled else { return nil }
        let incoming = MacSyncIdentity(
            server: state.identity.server,
            deviceID: state.identity.deviceID,
            credentialRevision: state.identity.credentialRevision
        )
        guard gate.accept(incoming, generation: state.generation) else { return nil }
        let connection: MacWindowFeature.Connection
        let legacyConnection: FleetStore.Connection
        if state.snapshot != nil && overview == nil {
            connection = .offline("Invalid response from alondrad")
            legacyConnection = .offline("Invalid response from alondrad")
        } else {
            switch state.connection {
            case .connecting:
                connection = .connecting
                legacyConnection = .connecting
            case .online:
                connection = .online
                legacyConnection = .online
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
        }
        if let overview, let body = state.snapshot?.body, body != lastSnapshotBody {
            fleet.apply(overview)
            lastSnapshotBody = body
        }
        fleet.setConnection(legacyConnection)
        triage?.send(.inboxUpdated(TriageInboxUpdate(
            identity: Self.triageIdentity(commandScope), overview: overview)))
        return MacWindowUpdate(
            identity: incoming,
            generation: state.generation,
            connection: connection,
            hasSnapshot: fleet.overview != nil,
            inboxIDs: overview == nil ? nil : triage?.queue.map(\.id)
        )
    }

    private static func triageIdentity(_ scope: CommandScope) -> TriageIdentity {
        TriageIdentity(server: scope.server, deviceID: scope.deviceID,
                       credentialRevision: scope.credentialRevision, generation: scope.generation)
    }
}

private enum MacTriageMutationError: Error { case unsupported }

@MainActor
final class MacWindowContext {
    let bridge: MacLegacyProjection
    let feature: StoreOf<MacWindowFeature>
    let triage: StoreOf<TriageFeature>

    var legacy: FleetStore { bridge.fleet }
    var drafts: Drafts { bridge.drafts }

    init(client: AlondraClient, services: MacAppSync) {
        let drafts = Drafts()
        let bridge = MacLegacyProjection(
            client: client,
            commandGate: services.commandGate,
            commandScope: services.commandScope,
            drafts: drafts
        )
        self.bridge = bridge
        triage = Store(initialState: TriageFeature.State()) {
            TriageFeature()
        } withDependencies: {
            $0.triageService = TriageService(
                observe: { AsyncStream { $0.finish() } },
                mutate: { kind, itemID, identity in
                    await services.mutateTriage(kind, itemID: itemID, identity: identity)
                }
            )
        }
        services.register(bridge)
        feature = Store(initialState: MacWindowFeature.State()) {
            MacWindowFeature()
        } withDependencies: {
            $0.macWindowSync = .live(session: services.session)
            $0.macWindowProjection = MacWindowProjection(project: { [bridge] state in
                let overview: Overview?
                if let snapshot = state.snapshot {
                    let decoder = JSONDecoder()
                    decoder.keyDecodingStrategy = .convertFromSnakeCase
                    overview = try? decoder.decode(Overview.self, from: snapshot.body)
                } else {
                    overview = nil
                }
                guard !Task.isCancelled else { return nil }
                return await bridge.apply(state, overview: overview)
            })
        }
        bridge.feature = feature
        bridge.triage = triage
    }
}
