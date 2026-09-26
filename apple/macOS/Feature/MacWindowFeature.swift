import ComposableArchitecture
import Foundation

/// A window owns navigation and presentation. The app owns the transport shared by all windows.
@Reducer
public struct MacWindowFeature {
    @ObservableState
    public struct State: Equatable {
        public let windowID: UUID
        public var selection: Selection?
        public var query = ""
        public var isNewRequestPresented = false
        public var newRequestOrigin: MacSyncIdentity?
        public var newRequestOriginEpoch: UInt64?
        public var presentationEpoch: UInt64 = 0
        public var pendingIdentity: MacSyncIdentity?
        public var requiredGeneration: UInt64 = 0
        public var connection: Connection = .connecting
        public var inboxIDs: [String] = []
        public var generation: UInt64 = 0
        public var identity: MacSyncIdentity?
        public var hasSnapshot = false
        public var pairingError: String?
        public var didAutoSelect = false

        public init(windowID: UUID = UUID()) {
            self.windowID = windowID
        }
    }

    public enum Connection: Equatable, Sendable {
        case connecting
        case online
        case offline(String)
        case unpaired
    }

    public enum Action: BindableAction {
        case binding(BindingAction<State>)
        case appeared
        case disappeared
        case identityWillChange(MacSyncIdentity)
        case received(MacWindowUpdate)
        case localInboxChanged([String])
        case pairingErrorChanged(String?)
        case dismissPairingError
        case startTriage
        case showNewRequest
        case dismissNewRequest
    }

    @Dependency(\.macWindowSync) var sync
    @Dependency(\.macWindowProjection) var projection

    private enum CancelID: Hashable { case subscription(UUID) }

    public init() {}

    public var body: some ReducerOf<Self> {
        BindingReducer()
        Reduce { state, action in
            switch action {
            case .binding:
                return .none
            case .appeared:
                let windowID = state.windowID
                return .run { send in
                    for await value in await sync.subscribe() {
                        try Task.checkCancellation()
                        if let update = await projection.project(value) {
                            await send(.received(update))
                        }
                    }
                }
                .cancellable(id: CancelID.subscription(windowID), cancelInFlight: true)
            case .disappeared:
                return .cancel(id: CancelID.subscription(state.windowID))
            case .identityWillChange(let expected):
                state.presentationEpoch &+= 1
                state.pendingIdentity = expected
                state.requiredGeneration = max(state.requiredGeneration, state.generation &+ 1)
                state.identity = nil
                state.connection = .connecting
                state.hasSnapshot = false
                state.inboxIDs = []
                state.selection = nil
                state.didAutoSelect = false
                state.query = ""
                state.isNewRequestPresented = false
                state.newRequestOrigin = nil
                state.newRequestOriginEpoch = nil
                return .none
            case .received(let update):
                // The stream may deliver one final value after cancellation or a credential
                // change. A complete value carries both generation and identity.
                guard update.generation >= state.generation else { return .none }
                if let pending = state.pendingIdentity {
                    guard update.identity == pending,
                          update.generation >= state.requiredGeneration else { return .none }
                    state.pendingIdentity = nil
                } else if let current = state.identity, update.identity != current {
                    guard update.generation > state.generation else { return .none }
                    state.presentationEpoch &+= 1
                }
                let changedIdentity = state.identity != update.identity
                if changedIdentity {
                    state.selection = nil
                    state.didAutoSelect = false
                    state.inboxIDs = []
                    state.query = ""
                    state.isNewRequestPresented = false
                    state.newRequestOrigin = nil
                    state.newRequestOriginEpoch = nil
                }
                let old = state.inboxIDs
                state.identity = update.identity
                state.generation = update.generation
                state.connection = update.connection
                state.hasSnapshot = update.hasSnapshot
                if let ids = update.inboxIDs {
                    state.inboxIDs = ids
                    state.selection = Self.follow(
                        selection: state.selection,
                        old: old,
                        new: ids,
                        didAutoSelect: &state.didAutoSelect
                    )
                }
                return .none
            case .localInboxChanged(let ids):
                guard state.pendingIdentity == nil, state.identity != nil else { return .none }
                guard ids != state.inboxIDs else { return .none }
                let old = state.inboxIDs
                state.inboxIDs = ids
                state.selection = Self.follow(
                    selection: state.selection,
                    old: old,
                    new: ids,
                    didAutoSelect: &state.didAutoSelect
                )
                return .none
            case .pairingErrorChanged(let error):
                state.pairingError = error
                return .none
            case .dismissPairingError:
                state.pairingError = nil
                return .none
            case .startTriage:
                state.selection = state.inboxIDs.first.map(Selection.item)
                return .none
            case .showNewRequest:
                state.newRequestOrigin = state.identity
                state.newRequestOriginEpoch = state.identity == nil ? nil : state.presentationEpoch
                state.isNewRequestPresented = state.newRequestOrigin != nil
                return .none
            case .dismissNewRequest:
                state.isNewRequestPresented = false
                state.newRequestOrigin = nil
                state.newRequestOriginEpoch = nil
                return .none
            }
        }
    }

    public static func canSubmitNewRequest(
        _ state: State, origin: MacSyncIdentity, epoch: UInt64
    ) -> Bool {
        state.isNewRequestPresented
            && state.pendingIdentity == nil
            && state.identity == origin
            && state.newRequestOrigin == origin
            && state.presentationEpoch == epoch
            && state.newRequestOriginEpoch == epoch
    }

    private static func follow(
        selection: Selection?, old: [String], new: [String], didAutoSelect: inout Bool
    ) -> Selection? {
        if !didAutoSelect, selection == nil, let first = new.first {
            didAutoSelect = true
            return .item(first)
        }
        guard case .item(let id)? = selection, !new.contains(id) else { return selection }
        let remaining = Set(new)
        guard let index = old.firstIndex(of: id) else { return new.first.map(Selection.item) }
        let next = old[(index + 1)...].first(where: remaining.contains)
            ?? old[..<index].last(where: remaining.contains)
        return next.map(Selection.item)
    }
}

public enum Selection: Hashable, Sendable {
    case item(String)
    case node(String)
    case task(String)
    case request(String)
    case schedules
}

public struct MacSyncIdentity: Hashable, Sendable {
    public let server: URL
    public let deviceID: String
    public let credentialRevision: String

    public init(server: URL, deviceID: String, credentialRevision: String) {
        self.server = server
        self.deviceID = deviceID
        self.credentialRevision = credentialRevision
    }
}

public struct MacWindowUpdate: Sendable {
    public let identity: MacSyncIdentity
    public let generation: UInt64
    public let connection: MacWindowFeature.Connection
    public let hasSnapshot: Bool
    /// Nil means retain the previous list during a temporary disconnect.
    public let inboxIDs: [String]?

    public init(
        identity: MacSyncIdentity,
        generation: UInt64,
        connection: MacWindowFeature.Connection,
        hasSnapshot: Bool,
        inboxIDs: [String]?
    ) {
        self.identity = identity
        self.generation = generation
        self.connection = connection
        self.hasSnapshot = hasSnapshot
        self.inboxIDs = inboxIDs
    }
}
