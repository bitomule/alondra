import AlondraContract
import ComposableArchitecture
import Foundation

public struct QuietHours: Equatable, Sendable {
    public var enabled: Bool
    public var start: Int
    public var end: Int
    public init(enabled: Bool = false, start: Int = 23 * 60, end: Int = 8 * 60) {
        self.enabled = enabled; self.start = start; self.end = end
    }
    public var valid: Bool { (0..<1440).contains(start) && (0..<1440).contains(end) }
}

public struct NotificationSettings: Equatable, Sendable {
    public enum Permission: Equatable, Sendable { case unknown, notDetermined, denied, authorized }
    public enum Delivery: Equatable, Sendable { case idle, registering, registered, failed(String) }
    public var permission: Permission
    public var delivery: Delivery
    public var quietHours: QuietHours
    public init(permission: Permission, delivery: Delivery, quietHours: QuietHours) {
        self.permission = permission; self.delivery = delivery; self.quietHours = quietHours
    }
}

public struct MacChoice: Identifiable, Equatable, Sendable {
    public let id: UUID
    public let macID: String
    public let name: String?
    /// Paired on the local network, without an account: it can be forgotten from here.
    public let local: Bool
    public init(id: UUID, macID: String, name: String? = nil, local: Bool = false) {
        self.id = id; self.macID = macID; self.name = name; self.local = local
    }
}

/// How a paired Mac answers right now, as a fact next to it.
public enum MacStatus: Equatable, Sendable {
    case route(ConnectionRoute)
    case connecting
    case problem(ConnectionProblem)

    public init(_ state: ConnectionState) {
        switch state {
        case .connected(let route): self = .route(route)
        case .connecting: self = .connecting
        case .unreachable(let problem): self = .problem(problem)
        }
    }
}

public struct SettingsClient: Sendable {
    public var observe: @Sendable () async -> AsyncStream<NotificationSettings>
    public var save: @Sendable (QuietHours, QuietHours) async throws -> Void
    public var refreshPermission: @Sendable () async -> Void
    public var requestPermission: @Sendable () async -> Void
    public var openSystemSettings: @Sendable () async -> Void
    public var checkConnection: @Sendable () async throws -> Void
    public var selectMac: @Sendable (UUID) async throws -> Void = { _ in throw CancellationError() }
    public var macStatuses: @Sendable () async -> [UUID: MacStatus] = { [:] }
    public var forgetMac: @Sendable (UUID) async throws -> Void = { _ in throw CancellationError() }
    /// Reads and changes one paired Mac's own settings; nil when that Mac has no session here.
    public var macSettings: @Sendable (UUID) async -> MacSettingsClient? = { _ in nil }
    /// Only the Mac app on its own daemon lists and revokes devices.
    public var devices: (@Sendable () async throws -> [TrustedDevice])?
    public var revokeDevice: @Sendable (String) async throws -> Void = { _ in throw CancellationError() }
    public var problem: @Sendable (any Error) -> ConnectionProblem = { _ in .unexpected }
    public var logout: @Sendable () async -> Void
    public init(observe: @escaping @Sendable () async -> AsyncStream<NotificationSettings>,
                save: @escaping @Sendable (QuietHours, QuietHours) async throws -> Void,
                refreshPermission: @escaping @Sendable () async -> Void = {},
                requestPermission: @escaping @Sendable () async -> Void,
                openSystemSettings: @escaping @Sendable () async -> Void,
                checkConnection: @escaping @Sendable () async throws -> Void,
                logout: @escaping @Sendable () async -> Void) {
        self.refreshPermission = refreshPermission
        self.observe = observe; self.save = save; self.requestPermission = requestPermission
        self.openSystemSettings = openSystemSettings; self.checkConnection = checkConnection; self.logout = logout
    }
}
extension SettingsClient: DependencyKey {
    public static let liveValue = Self(observe: { AsyncStream { $0.finish() } },
        save: { _, _ in preconditionFailure("Install settings adapter") }, requestPermission: {}, openSystemSettings: {},
        checkConnection: { preconditionFailure("Install settings adapter") }, logout: {})
}
public extension DependencyValues {
    var serviceSettings: SettingsClient {
        get { self[SettingsClient.self] }
        set { self[SettingsClient.self] = newValue }
    }
}

@Reducer public struct SettingsFeature: Sendable {
    @ObservableState public struct State: Equatable {
        public let service: String
        public let device: String
        public let supportsNotifications: Bool
        public let usesAccount: Bool
        public let automaticallySavesQuietHours: Bool
        public var autosaveRevision: UInt64 = 0
        public var autosaveDraft: QuietHours?
        public var notifications: NotificationSettings?
        public var draft = QuietHours()
        public var baseline = QuietHours()
        public var editing = false
        public var conflict = false
        public var saving = false
        public var checking = false
        public var connected = false
        public var connectionError: ConnectionProblem?
        public var error: String?
        public var pairingCleanup = false
        @Presents public var pairing: SignInFeature.State?
        @Presents public var macSettings: MacSettingsFeature.State?
        public var macs: [MacChoice] = []
        public var selectedMacID: UUID?
        public var selectingMacID: UUID?
        public var macError: ConnectionProblem?
        public var forgetError: String?
        public var macStatuses: [UUID: MacStatus] = [:]
        public var forgettingMacID: UUID?
        public var confirmForgetMacID: UUID?
        public var devices: [TrustedDevice]?
        public var devicesError: String?
        public var revokingDeviceID: String?
        public var confirmRevokeDeviceID: String?
        /// The Mac app on its own daemon, with an account behind some of its paired Macs.
        public var showsSignOutWithMacs = false
        public var confirmLogout = false
        public var machineSettings = MachineSettingsFeature.State()
        public var allowedRules = AllowedRulesFeature.State()
        public var accounts = ServerSettingsFeature.State(key: .accounts)
        public var mcpServers = ServerSettingsFeature.State(key: .mcpServers)
        public var measurements = ConnectionMeasurementsFeature.State()
        public var generation: UInt64 = 0
        public let cancellation = UUID()
        public init(service: String, device: String, supportsNotifications: Bool, automaticallySavesQuietHours: Bool = false,
                    usesAccount: Bool = true) {
            self.automaticallySavesQuietHours = automaticallySavesQuietHours
            self.usesAccount = usesAccount
            self.service = service; self.device = device; self.supportsNotifications = supportsNotifications
        }
        public var canSave: Bool { editing && !saving && !conflict && draft.valid }
    }
    public enum Action {
        case appeared, disappeared
        case updated(NotificationSettings)
        case quietEnabled(Bool), quietStart(Int), quietEnd(Int)
        case useCurrent, save, saved(UInt64), failed(String, UInt64)
        case autosaveFinished(QuietHours, UInt64, String?)
        case refreshPermission, requestPermission, openSystemSettings
        case checkConnection, checked(UInt64), checkFailed(ConnectionProblem, UInt64)
        case linkMac, pairingCleanupFinished, pairing(PresentationAction<SignInFeature.Action>)
        case selectMac(UUID), macSelected(UUID, UInt64), macSelectionFailed(ConnectionProblem?, UInt64)
        case loadMacStatuses, macStatusesLoaded([UUID: MacStatus])
        case openMacSettings(UUID), macSettingsReady(UUID, MacSettingsClient), macSettings(PresentationAction<MacSettingsFeature.Action>)
        case forgetMacTapped(UUID), cancelForgetMac, confirmForgetMac, macForgotten(UUID), macForgetFailed(String)
        case loadDevices, devicesLoaded([TrustedDevice]), devicesFailed(String)
        case revokeDeviceTapped(String), cancelRevokeDevice, confirmRevokeDevice, deviceRevoked(String), revokeFailed(String)
        case logoutTapped, cancelLogout, confirmLogout
        case machineSettings(MachineSettingsFeature.Action)
        case allowedRules(AllowedRulesFeature.Action)
        case accounts(ServerSettingsFeature.Action)
        case mcpServers(ServerSettingsFeature.Action)
        case measurements(ConnectionMeasurementsFeature.Action)
    }
    @Dependency(\.serviceSettings) var client
    @Dependency(\.cloudPairing) var pairingService
    @Dependency(\.findMac) var findMacService
    private enum StatusesID { case statuses }
    public init() {}
    static func saveFailure(_ error: any Error) -> String {
        (error as? LocalizedError)?.errorDescription
            ?? String(localized: "Your notification settings couldn’t be saved. Try again.")
    }

    public var body: some ReducerOf<Self> {
        Scope(state: \.machineSettings, action: \.machineSettings) { MachineSettingsFeature() }
        Scope(state: \.allowedRules, action: \.allowedRules) { AllowedRulesFeature() }
        Scope(state: \.accounts, action: \.accounts) { ServerSettingsFeature() }
        Scope(state: \.mcpServers, action: \.mcpServers) { ServerSettingsFeature() }
        Scope(state: \.measurements, action: \.measurements) { ConnectionMeasurementsFeature() }
        Reduce { state, action in
            switch action {
            case .machineSettings, .allowedRules, .accounts, .mcpServers, .measurements: return .none
            case .appeared:
                guard state.supportsNotifications else { return .none }
                return .run { send in
                    for await snapshot in await client.observe() { await send(.updated(snapshot)) }
                }.cancellable(id: state.cancellation, cancelInFlight: true)
            case .loadMacStatuses:
                return .run { send in await send(.macStatusesLoaded(await client.macStatuses())) }
                    .cancellable(id: StatusesID.statuses, cancelInFlight: true)
            case .macStatusesLoaded(let statuses):
                state.macStatuses = statuses
                return .none
            case .forgetMacTapped(let id):
                guard state.macs.contains(where: { $0.id == id && $0.local }), state.forgettingMacID == nil else { return .none }
                state.confirmForgetMacID = id
                return .none
            case .cancelForgetMac:
                state.confirmForgetMacID = nil
                return .none
            case .confirmForgetMac:
                guard let id = state.confirmForgetMacID else { return .none }
                state.confirmForgetMacID = nil
                state.forgettingMacID = id
                state.forgetError = nil
                return .run { send in
                    do { try await client.forgetMac(id); await send(.macForgotten(id)) }
                    catch { await send(.macForgetFailed(String(localized: "Alondra couldn’t forget this Mac. Try again."))) }
                }
            case .macForgotten(let id):
                state.forgettingMacID = nil
                state.macs.removeAll { $0.id == id }
                return .none
            case .macForgetFailed(let message):
                state.forgettingMacID = nil
                state.forgetError = message
                return .none
            case .loadDevices:
                guard let devices = client.devices else { return .none }
                return .run { send in
                    do { await send(.devicesLoaded(try await devices())) }
                    catch { await send(.devicesFailed(String(localized: "Alondra couldn’t read the devices of this Mac."))) }
                }
            case .devicesLoaded(let devices):
                state.devices = devices.filter { $0.origin != .mac }
                state.devicesError = nil
                return .none
            case .devicesFailed(let message):
                state.devicesError = message
                return .none
            case .revokeDeviceTapped(let id):
                guard state.revokingDeviceID == nil else { return .none }
                state.confirmRevokeDeviceID = id
                return .none
            case .cancelRevokeDevice:
                state.confirmRevokeDeviceID = nil
                return .none
            case .confirmRevokeDevice:
                guard let id = state.confirmRevokeDeviceID else { return .none }
                state.confirmRevokeDeviceID = nil
                state.revokingDeviceID = id
                state.devices?.removeAll { $0.id == id }
                return .run { send in
                    do { try await client.revokeDevice(id); await send(.deviceRevoked(id)) }
                    catch { await send(.revokeFailed(String(localized: "Alondra couldn’t revoke this device. Try again."))) }
                }
            case .deviceRevoked:
                state.revokingDeviceID = nil
                return .none
            case .revokeFailed(let message):
                state.revokingDeviceID = nil
                state.devicesError = message
                return .send(.loadDevices)
            case .disappeared:
                state.generation &+= 1
                if !state.automaticallySavesQuietHours { state.saving = false }
                state.checking = false
                state.selectingMacID = nil
                return .cancel(id: state.cancellation)
            case .updated(let snapshot):
                state.notifications = snapshot
                if let submitted = state.autosaveDraft {
                    // Own observation echoes are not a foreign writer; keep newer local edits.
                    if snapshot.quietHours != state.baseline && snapshot.quietHours != submitted { state.conflict = true }
                } else if !state.editing { state.baseline = snapshot.quietHours; state.draft = snapshot.quietHours }
                else if snapshot.quietHours != state.baseline { state.conflict = snapshot.quietHours != state.draft }
                return .none
            case .quietEnabled(let enabled):
                guard !state.saving || state.automaticallySavesQuietHours else { return .none }
                state.draft.enabled = enabled; state.editing = state.draft != state.baseline
                return state.automaticallySavesQuietHours ? .send(.save) : .none
            case .quietStart(let minutes):
                guard !state.saving || state.automaticallySavesQuietHours else { return .none }
                state.draft.start = minutes; state.editing = state.draft != state.baseline
                return state.automaticallySavesQuietHours ? .send(.save) : .none
            case .quietEnd(let minutes):
                guard !state.saving || state.automaticallySavesQuietHours else { return .none }
                state.draft.end = minutes; state.editing = state.draft != state.baseline
                return state.automaticallySavesQuietHours ? .send(.save) : .none
            case .useCurrent:
                guard !state.saving, let snapshot = state.notifications else { return .none }
                state.baseline = snapshot.quietHours; state.draft = snapshot.quietHours
                state.editing = false; state.conflict = false; state.error = nil; return .none
            case .save:
                guard state.canSave else { return .none }
                if state.automaticallySavesQuietHours {
                    state.saving = true; state.error = nil
                    state.autosaveRevision &+= 1
                    state.autosaveDraft = state.draft
                    let submitted = state.draft, baseline = state.baseline, revision = state.autosaveRevision
                    // The scene owns this store beyond Settings dismissal, so an accepted
                    // edit and its queued successor are not lost when the sheet closes.
                    return .run { send in
                        do { try await client.save(submitted, baseline); await send(.autosaveFinished(submitted, revision, nil)) }
                        catch { await send(.autosaveFinished(submitted, revision, Self.saveFailure(error))) }
                    }
                }
                state.saving = true; state.error = nil
                let draft = state.draft, baseline = state.baseline, generation = state.generation
                return .run { send in
                    do { try await client.save(draft, baseline); await send(.saved(generation)) }
                    catch { await send(.failed(Self.saveFailure(error), generation)) }
                }.cancellable(id: state.cancellation)
            case .autosaveFinished(let submitted, let revision, let error):
                guard state.autosaveRevision == revision, state.autosaveDraft == submitted else { return .none }
                state.autosaveDraft = nil; state.saving = false
                if let error { state.error = error; return .none }
                state.baseline = submitted
                state.editing = state.draft != submitted
                // A foreign snapshot observed during save stays a conflict. Never rebase
                // a queued edit onto another window's preferences without user action.
                return state.editing && !state.conflict ? .send(.save) : .none
            case .saved(let generation):
                guard generation == state.generation else { return .none }
                state.saving = false
                if let latest = state.notifications?.quietHours, latest != state.baseline { state.draft = latest }
                state.baseline = state.draft; state.editing = false; state.conflict = false
                return .none
            case .failed(let message, let generation):
                guard generation == state.generation else { return .none }
                state.saving = false; state.checking = false; state.error = message; return .none
            case .refreshPermission: return .run { _ in await client.refreshPermission() }
            case .requestPermission: return .run { _ in await client.requestPermission() }
            case .openSystemSettings: return .run { _ in await client.openSystemSettings() }
            case .checkConnection:
                guard !state.checking else { return .none }
                state.checking = true; state.connected = false; state.connectionError = nil
                let generation = state.generation
                return .run { send in
                    do { try await client.checkConnection(); await send(.checked(generation)) }
                    catch { await send(.checkFailed(client.problem(error), generation)) }
                }.cancellable(id: state.cancellation)
            case .checked(let generation):
                guard generation == state.generation else { return .none }
                state.checking = false; state.connected = true; return .none
            case .checkFailed(let problem, let generation):
                guard generation == state.generation else { return .none }
                state.checking = false; state.connectionError = problem
                return .none
            case .linkMac:
                guard state.selectingMacID == nil, !state.pairingCleanup else { return .none }
                state.pairing = SignInFeature.State(additionalMac: true)
                return .none
            case .pairing(.dismiss):
                state.pairingCleanup = true
                return .run { send in
                    await pairingService.cancel()
                    await findMacService.cancel()
                    await send(.pairingCleanupFinished)
                }
            case .pairing(.presented(.findMac(.delegate(.paired)))):
                state.pairing = nil
                return .send(.loadMacStatuses)
            case .pairingCleanupFinished:
                state.pairingCleanup = false
                return .none
            case .pairing: return .none
            case .selectMac(let id):
                guard state.selectingMacID == nil, id != state.selectedMacID,
                      state.macs.contains(where: { $0.id == id }) else { return .none }
                state.selectingMacID = id; state.macError = nil
                let generation = state.generation
                return .run { send in
                    do { try await client.selectMac(id); await send(.macSelected(id, generation)) }
                    catch { await send(.macSelectionFailed(error.isCancellation ? nil : client.problem(error), generation)) }
                }.cancellable(id: state.cancellation)
            case .macSelected(let id, let generation):
                guard generation == state.generation, state.selectingMacID == id else { return .none }
                state.selectedMacID = id; state.selectingMacID = nil
                return .none
            case .macSelectionFailed(let problem, let generation):
                guard generation == state.generation, state.selectingMacID != nil else { return .none }
                state.selectingMacID = nil; state.macError = problem
                return .none
            case .openMacSettings(let id):
                return .run { send in
                    if let settings = await client.macSettings(id) { await send(.macSettingsReady(id, settings)) }
                }
            case .macSettingsReady(let id, let settings):
                guard let mac = state.macs.first(where: { $0.id == id }) else { return .none }
                state.macSettings = MacSettingsFeature.State(id: id, macName: mac.displayName, client: settings)
                return .none
            case .macSettings: return .none
            case .logoutTapped: state.confirmLogout = true; return .none
            case .cancelLogout: state.confirmLogout = false; return .none
            case .confirmLogout:
                state.confirmLogout = false
                return .run { _ in await client.logout() }
            }
        }
        .ifLet(\.$pairing, action: \.pairing) { SignInFeature() }
        .ifLet(\.$macSettings, action: \.macSettings) { MacSettingsFeature() }
    }
}
