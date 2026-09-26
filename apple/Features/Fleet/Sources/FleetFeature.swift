import AlondraDomain
import AlondraShared
import ComposableArchitecture
import Foundation

@Reducer
public struct FleetFeature {
    @ObservableState
    public struct State: Equatable {
        public var snapshot: FleetSnapshot?
        public var isStale = false
        public var query = ""
        public var selectedNodeID: String?
        public var closeConfirmationID: String?
        public var closeRefusal: String?
        public var closeRequest: CloseRequest? {
            guard let id = closeConfirmationID, let entry = snapshot?.node(id) else { return nil }
            return CloseRequest(session: id, name: entry.node.displayName)
        }
        public var closingIDs: Set<String> = []
        public var closeOutcomeByID: [String: NodeMutationOutcome] = [:]
        public var presentationEpoch: UInt64 = 0
        public let cancellationID = UUID()

        public init() {}

        public var filteredSections: [FleetSection] {
            snapshot.map { ProjectFleetUseCase().search($0, query: query) } ?? []
        }

        public var selectedNode: FleetNode? { selectedNodeID.flatMap { snapshot?.node($0) } }
    }

    public enum Action {
        case snapshotChanged(FleetSnapshot)
        case snapshotUnavailable
        case identityChanged
        case searchChanged(String)
        case selectNode(String?)
        case closeRequested(String)
        case dismissCloseRefusal
        case closeCancelled
        case closeConfirmed
        case closeFinished(String, NodeMutationOutcome, UInt64)
        case commandResolved(UUID, rejection: String?)
        case commandReconciled(UUID)
    }

    @Dependency(\.nodeOperations) var operations
    public init() {}

    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .snapshotChanged(let snapshot):
                guard state.snapshot == nil || snapshot.sequence > state.snapshot!.sequence else { return .none }
                state.snapshot = snapshot
                state.isStale = false
                if let id = state.selectedNodeID, snapshot.node(id) == nil { state.selectedNodeID = nil }
                if let id = state.closeConfirmationID, snapshot.node(id) == nil { state.closeConfirmationID = nil }
                return .none
            case .snapshotUnavailable:
                state.isStale = true
                return .none
            case .identityChanged:
                state.presentationEpoch &+= 1
                state.snapshot = nil
                state.isStale = false
                state.selectedNodeID = nil
                state.closeConfirmationID = nil
                state.closingIDs = []
                state.closeOutcomeByID = [:]
                state.closeRefusal = nil
                return .cancel(id: state.cancellationID)
            case .searchChanged(let query):
                state.query = query
                return .none
            case .selectNode(let id):
                state.selectedNodeID = id.flatMap { state.snapshot?.node($0) != nil ? $0 : nil }
                return .none
            case .dismissCloseRefusal:
                state.closeRefusal = nil
                return .none
            case .closeRequested(let id):
                guard !state.isStale, state.snapshot?.node(id) != nil,
                      !state.closingIDs.contains(id), state.closeOutcomeByID[id]?.uncertainID == nil,
                      state.closeOutcomeByID[id] != .confirmed else { return .none }
                state.closeConfirmationID = id
                return .none
            case .closeCancelled:
                state.closeConfirmationID = nil
                return .none
            case .closeConfirmed:
                guard let id = state.closeConfirmationID, !state.isStale,
                      state.snapshot?.node(id) != nil, !state.closingIDs.contains(id),
                      state.closeOutcomeByID[id]?.uncertainID == nil,
                      state.closeOutcomeByID[id] != .confirmed else { return .none }
                state.closeConfirmationID = nil
                state.closingIDs.insert(id)
                state.closeOutcomeByID[id] = nil
                let epoch = state.presentationEpoch
                return .run { send in
                    await send(.closeFinished(id, await operations.close(id), epoch))
                }.cancellable(id: state.cancellationID)
            case .closeFinished(let id, let outcome, let epoch):
                guard epoch == state.presentationEpoch, state.closingIDs.remove(id) != nil else { return .none }
                state.closeOutcomeByID[id] = outcome
                if case .rejected(let message) = outcome { state.closeRefusal = message }
                if outcome == .confirmed, state.selectedNodeID == id { state.selectedNodeID = nil }
                return .none
            case .commandResolved(let id, let rejection):
                for (node, outcome) in state.closeOutcomeByID where outcome.uncertainID == id {
                    state.closeOutcomeByID[node] = rejection.map(NodeMutationOutcome.rejected) ?? .confirmed
                    if let rejection { state.closeRefusal = rejection }
                    if rejection == nil, state.selectedNodeID == node { state.selectedNodeID = nil }
                }
                return .none
            case .commandReconciled(let commandID):
                state.closeOutcomeByID = state.closeOutcomeByID.filter { $0.value.uncertainID != commandID }
                return .none
            }
        }
    }
}
