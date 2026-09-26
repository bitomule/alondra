import ComposableArchitecture
import Foundation

public struct ServiceQuietHours: Equatable, Sendable {
    public var enabled: Bool
    public var start: Int
    public var end: Int
    public init(enabled: Bool = false, start: Int = 23 * 60, end: Int = 8 * 60) {
        self.enabled = enabled; self.start = start; self.end = end
    }
    public var valid: Bool { (0..<1440).contains(start) && (0..<1440).contains(end) }
}

public struct ServiceNotificationSettings: Equatable, Sendable {
    public enum Permission: Equatable, Sendable { case unknown, notDetermined, denied, authorized }
    public enum Delivery: Equatable, Sendable { case idle, registering, registered, failed(String) }
    public var permission: Permission
    public var delivery: Delivery
    public var quietHours: ServiceQuietHours
    public init(permission: Permission, delivery: Delivery, quietHours: ServiceQuietHours) {
        self.permission = permission; self.delivery = delivery; self.quietHours = quietHours
    }
}

public struct ServiceSettingsClient: Sendable {
    public var observe: @Sendable () async -> AsyncStream<ServiceNotificationSettings>
    public var save: @Sendable (ServiceQuietHours, ServiceQuietHours) async throws -> Void
    public var refreshPermission: @Sendable () async -> Void
    public var requestPermission: @Sendable () async -> Void
    public var openSystemSettings: @Sendable () async -> Void
    public var checkConnection: @Sendable () async throws -> Void
    public var logout: @Sendable () async -> Void
    public init(observe: @escaping @Sendable () async -> AsyncStream<ServiceNotificationSettings>,
                save: @escaping @Sendable (ServiceQuietHours, ServiceQuietHours) async throws -> Void,
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
extension ServiceSettingsClient: DependencyKey {
    public static let liveValue = Self(observe: { AsyncStream { $0.finish() } },
        save: { _, _ in preconditionFailure("Install settings adapter") }, requestPermission: {}, openSystemSettings: {},
        checkConnection: { preconditionFailure("Install settings adapter") }, logout: {})
}
public extension DependencyValues {
    var serviceSettings: ServiceSettingsClient {
        get { self[ServiceSettingsClient.self] }
        set { self[ServiceSettingsClient.self] = newValue }
    }
}

@Reducer public struct ServiceSettingsFeature {
    @ObservableState public struct State: Equatable {
        public let service: String
        public let device: String
        public let supportsNotifications: Bool
        public var notifications: ServiceNotificationSettings?
        public var draft = ServiceQuietHours()
        public var baseline = ServiceQuietHours()
        public var editing = false
        public var conflict = false
        public var saving = false
        public var checking = false
        public var connected = false
        public var connectionError: String?
        public var error: String?
        public var confirmLogout = false
        public var generation: UInt64 = 0
        public let cancellation = UUID()
        public init(service: String, device: String, supportsNotifications: Bool) {
            self.service = service; self.device = device; self.supportsNotifications = supportsNotifications
        }
        public var canSave: Bool { editing && !saving && !conflict && draft.valid }
    }
    public enum Action {
        case appeared, disappeared
        case updated(ServiceNotificationSettings)
        case quietEnabled(Bool), quietStart(Int), quietEnd(Int)
        case useCurrent, save, saved(UInt64), failed(String, UInt64)
        case refreshPermission, requestPermission, openSystemSettings
        case checkConnection, checked(UInt64), checkFailed(String, UInt64)
        case logoutTapped, cancelLogout, confirmLogout
    }
    @Dependency(\.serviceSettings) var client
    public init() {}
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
                state.saving = false; state.checking = false
                return .cancel(id: state.cancellation)
            case .updated(let snapshot):
                state.notifications = snapshot
                if !state.editing { state.baseline = snapshot.quietHours; state.draft = snapshot.quietHours }
                else if snapshot.quietHours != state.baseline { state.conflict = snapshot.quietHours != state.draft }
                return .none
            case .quietEnabled(let enabled):
                guard !state.saving else { return .none }
                state.draft.enabled = enabled; state.editing = state.draft != state.baseline; return .none
            case .quietStart(let minutes):
                guard !state.saving else { return .none }
                state.draft.start = minutes; state.editing = state.draft != state.baseline; return .none
            case .quietEnd(let minutes):
                guard !state.saving else { return .none }
                state.draft.end = minutes; state.editing = state.draft != state.baseline; return .none
            case .useCurrent:
                guard !state.saving, let snapshot = state.notifications else { return .none }
                state.baseline = snapshot.quietHours; state.draft = snapshot.quietHours
                state.editing = false; state.conflict = false; state.error = nil; return .none
            case .save:
                guard state.canSave else { return .none }
                state.saving = true; state.error = nil
                let draft = state.draft, baseline = state.baseline, generation = state.generation
                return .run { send in
                    do { try await client.save(draft, baseline); await send(.saved(generation)) }
                    catch { await send(.failed(error.localizedDescription, generation)) }
                }.cancellable(id: state.cancellation)
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
                    catch { await send(.checkFailed(error.localizedDescription, generation)) }
                }.cancellable(id: state.cancellation)
            case .checked(let generation):
                guard generation == state.generation else { return .none }
                state.checking = false; state.connected = true; return .none
            case .checkFailed(let message, let generation):
                guard generation == state.generation else { return .none }
                state.checking = false; state.connectionError = message
                return .none
            case .logoutTapped: state.confirmLogout = true; return .none
            case .cancelLogout: state.confirmLogout = false; return .none
            case .confirmLogout:
                state.confirmLogout = false
                return .run { _ in await client.logout() }
            }
        }
    }
}
