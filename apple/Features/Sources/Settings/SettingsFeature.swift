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
    public init(id: UUID, macID: String) { self.id = id; self.macID = macID }
}

public struct SettingsClient: Sendable {
    public var observe: @Sendable () async -> AsyncStream<NotificationSettings>
    public var save: @Sendable (QuietHours, QuietHours) async throws -> Void
    public var refreshPermission: @Sendable () async -> Void
    public var requestPermission: @Sendable () async -> Void
    public var openSystemSettings: @Sendable () async -> Void
    public var checkConnection: @Sendable () async throws -> Void
    public var selectMac: @Sendable (UUID) async throws -> Void = { _ in throw CancellationError() }
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
        public var macs: [MacChoice] = []
        public var selectedMacID: UUID?
        public var selectingMacID: UUID?
        public var macError: ConnectionProblem?
        public var confirmLogout = false
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
        case logoutTapped, cancelLogout, confirmLogout
    }
    @Dependency(\.serviceSettings) var client
    @Dependency(\.cloudPairing) var pairingService
    public init() {}
    static func saveFailure(_ error: any Error) -> String {
        (error as? LocalizedError)?.errorDescription
            ?? String(localized: "Your notification settings couldn’t be saved. Try again.")
    }

    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .appeared:
                guard state.supportsNotifications else { return .none }
                return .run { send in
                    for await snapshot in await client.observe() { await send(.updated(snapshot)) }
                }.cancellable(id: state.cancellation, cancelInFlight: true)
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
                    await send(.pairingCleanupFinished)
                }
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
            case .logoutTapped: state.confirmLogout = true; return .none
            case .cancelLogout: state.confirmLogout = false; return .none
            case .confirmLogout:
                state.confirmLogout = false
                return .run { _ in await client.logout() }
            }
        }
        .ifLet(\.$pairing, action: \.pairing) { SignInFeature() }
    }
}
