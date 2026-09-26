import ComposableArchitecture
import Foundation

/// Navigation and connection state belong to the scene. The process owns the poll session.
@Reducer
public struct IOSRootFeature {
    @ObservableState
    public struct State: Equatable {
        public var tab: Tab = .triage
        public var connection: Connection = .connecting
        public var inboxCount = 0
        public var identity: IOSSyncIdentity?
        public var generation: UInt64 = 0
        public var presentationGeneration: UInt64 = 0
        public var hasSnapshot = false
        public var isActive = false
        public var isReportingActivity = false
        /// Cancellation IDs are global to the dependency context, so each scene needs its own.
        public var subscriptionID = UUID()

        public init() {}
    }

    public enum Tab: Hashable, Sendable {
        case triage, tasks, fleet, new
    }

    public enum Connection: Equatable, Sendable {
        case connecting, online, offline(String), unpaired
    }

    public enum Action: BindableAction {
        case binding(BindingAction<State>)
        case sceneActive(Bool)
        case activityActive(UUID, Bool)
        case configurationChanged(String)
        case pairingChanged
        case received(IOSRootUpdate)
        case inboxCountChanged(Int)
        case focusTriage
    }

    @Dependency(\.iosRootSync) var sync
    @Dependency(\.iosRootProjection) var projection
    @Dependency(\.iosRootLifecycle) var lifecycle

    public init() {}

    public var body: some ReducerOf<Self> {
        BindingReducer()
        Reduce { state, action in
            switch action {
            case .binding:
                return .none
            case .sceneActive(let active):
                guard state.isActive != active else { return .none }
                state.isActive = active
                let subscriptionID = state.subscriptionID
                guard active else { return .cancel(id: subscriptionID) }
                return .run { send in
                    for await value in await sync.subscribe() {
                        try Task.checkCancellation()
                        if let update = await projection.project(value) {
                            await send(.received(update))
                        }
                    }
                }
                .cancellable(id: subscriptionID, cancelInFlight: true)
            case .activityActive(let sceneID, let active):
                guard state.isReportingActivity != active else { return .none }
                state.isReportingActivity = active
                return .merge(
                    .run { _ in await lifecycle.setForegroundActivity(sceneID, active) },
                    active ? .run { _ in await lifecycle.refreshAuthorization() } : .none
                )
            case .configurationChanged(let address):
                return .run { _ in await lifecycle.configure(address) }
            case .pairingChanged:
                return .run { _ in await lifecycle.activatePush() }
            case .received(let update):
                guard state.isActive, update.generation >= state.generation else { return .none }
                if state.identity != update.identity || update.resetsPresentation {
                    state.tab = .triage
                    state.inboxCount = 0
                    state.presentationGeneration &+= 1
                }
                state.identity = update.identity
                state.generation = update.generation
                state.connection = update.connection
                state.hasSnapshot = update.hasSnapshot
                if let count = update.inboxCount { state.inboxCount = count }
                return .none
            case .inboxCountChanged(let count):
                state.inboxCount = count
                return .none
            case .focusTriage:
                state.tab = .triage
                return .none
            }
        }
    }
}

public struct IOSSyncIdentity: Equatable, Sendable {
    public let server: URL
    public let deviceID: String
    public let credentialRevision: String

    public init(server: URL, deviceID: String, credentialRevision: String) {
        self.server = server
        self.deviceID = deviceID
        self.credentialRevision = credentialRevision
    }
}

public struct IOSRootUpdate: Sendable {
    public let identity: IOSSyncIdentity
    public let generation: UInt64
    public let connection: IOSRootFeature.Connection
    public let hasSnapshot: Bool
    /// Nil retains the last count during a temporary disconnect.
    public let inboxCount: Int?
    public let resetsPresentation: Bool

    public init(identity: IOSSyncIdentity, generation: UInt64, connection: IOSRootFeature.Connection, hasSnapshot: Bool, inboxCount: Int?, resetsPresentation: Bool = false) {
        self.identity = identity
        self.generation = generation
        self.connection = connection
        self.hasSnapshot = hasSnapshot
        self.inboxCount = inboxCount
        self.resetsPresentation = resetsPresentation
    }
}
