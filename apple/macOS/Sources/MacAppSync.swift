import AlondraAPIClient
import AlondraCommands
import AlondraDeviceIdentity
import AlondraMacFeature
import AlondraShared
import AlondraSync
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

    init(client: AlondraClient) {
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
        commandGate = CommandGate(scope: scope)
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
        return MacWindowUpdate(
            identity: incoming,
            generation: state.generation,
            connection: connection,
            hasSnapshot: fleet.overview != nil,
            inboxIDs: overview == nil ? nil : fleet.inbox.map(\.id)
        )
    }
}

@MainActor
final class MacWindowContext {
    let bridge: MacLegacyProjection
    let feature: StoreOf<MacWindowFeature>

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
    }
}

/// The sheet can outlive its original server in an already scheduled Task. Reject its draft
/// at the call boundary before FleetStore can capture a newly paired client.
@MainActor
struct MacNewRequestSubmission {
    let feature: StoreOf<MacWindowFeature>
    let fleet: FleetStore
    let services: MacAppSync
    let origin: MacSyncIdentity?
    let originEpoch: UInt64?
    let originScope: CommandScope

    func call(_ body: NewRequestBody, attachments: Attachments) async -> Bool {
        guard let origin,
              let originEpoch,
              services.commandScope == originScope,
              MacWindowFeature.canSubmitNewRequest(feature.state, origin: origin, epoch: originEpoch)
        else { return false }
        return await fleet.newRequest(body, attachments: attachments)
    }
}

struct MacWindowScene: View {
    let device: LocalDevice
    let services: MacAppSync
    @State private var context: MacWindowContext

    init(device: LocalDevice, services: MacAppSync) {
        self.device = device
        self.services = services
        _context = State(initialValue: MacWindowContext(client: device.client, services: services))
    }

    var body: some View {
        FleetView(store: context.legacy, feature: context.feature, services: services, drafts: context.drafts)
            .id(context.feature.identity)
            .environment(services)
            .frame(minWidth: 860, minHeight: 540)
            .environment(device)
            .task { context.feature.send(.appeared) }
            .task { services.startActivityMonitoring() }
            .onChange(of: context.legacy.inbox.map(\.id)) { _, ids in
                context.feature.send(.localInboxChanged(ids))
            }
            .onChange(of: services.pairingError, initial: true) { _, error in
                context.feature.send(.pairingErrorChanged(error))
            }
            .onDisappear { context.feature.send(.disappeared) }
            .onChange(of: MacAppSync.identity(for: device.client)) { _, _ in
                let scope = services.prepareUse(device.client)
                context.bridge.use(device.client, commandScope: scope)
                // A window may observe the shared session's new identity before this scene's
                // onChange runs and reject that event. Resubscription replays its current state.
                context.feature.send(.disappeared)
                context.feature.send(.appeared)
                Task { await services.activate(device.client, scope: scope) }
            }
    }
}
