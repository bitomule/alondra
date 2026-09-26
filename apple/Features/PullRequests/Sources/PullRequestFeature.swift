import AlondraShared
import ComposableArchitecture
import Foundation

@Reducer
public struct PullRequestFeature {
    @ObservableState
    public struct State: Equatable {
        public let id: UInt64
        public var pr: PullRequest?
        public var isClosed = false
        public var isStale = false
        public var selectedMergeMethod: String?
        public var isSending = false
        public var mutationOutcome: PRMutationOutcome?
        public var shouldDismiss = false
        public var successfulActionCount = 0
        public var presentationEpoch: UInt64 = 0
        public let mutationID = UUID()

        public init(pr: PullRequest) {
            id = pr.id
            self.pr = pr
            isClosed = pr.state != "open"
        }

        public var canAct: Bool {
            pr != nil && !isClosed && !isStale && !isSending
                && mutationOutcome?.uncertainID == nil && mutationOutcome != .confirmed
        }

        public var checks: [PRCheck] { pr?.checks ?? [] }
    }

    public enum Action {
        case pullRequestChanged(PullRequest?)
        case snapshotUnavailable
        case identityChanged
        case chooseMergeMethod(String)
        case cancelMerge
        case confirmMerge
        case resolveConflictsTapped
        case setAsideTapped
        case mutationFinished(PRMutationOutcome, UInt64)
        case commandResolved(UUID, rejection: String?)
        case commandReconciled(UUID)
    }

    @Dependency(\.pullRequestOperations) var operations
    private let mergeRule = PRMergeUseCase()

    public init() {}

    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .pullRequestChanged(let pr):
                guard pr?.id == state.id || pr == nil else { return .none }
                state.pr = pr
                state.isClosed = pr?.state != "open"
                state.isStale = false
                if !mergeRule.allows(state.selectedMergeMethod ?? "", on: pr) {
                    state.selectedMergeMethod = nil
                }
                return .none
            case .snapshotUnavailable:
                state.isStale = true
                return .none
            case .identityChanged:
                state.presentationEpoch &+= 1
                state.pr = nil
                state.isClosed = true
                state.isStale = false
                state.selectedMergeMethod = nil
                state.isSending = false
                state.mutationOutcome = nil
                state.shouldDismiss = false
                return .cancel(id: state.mutationID)
            case .chooseMergeMethod(let method):
                guard state.canAct, mergeRule.allows(method, on: state.pr) else { return .none }
                state.selectedMergeMethod = method
                return .none
            case .cancelMerge:
                state.selectedMergeMethod = nil
                return .none
            case .confirmMerge:
                guard state.canAct, let method = state.selectedMergeMethod,
                      mergeRule.allows(method, on: state.pr) else {
                    state.selectedMergeMethod = nil
                    return .none
                }
                state.selectedMergeMethod = nil
                state.isSending = true
                state.mutationOutcome = nil
                let id = state.id
                let epoch = state.presentationEpoch
                return .run { send in await send(.mutationFinished(await operations.merge(id, method), epoch)) }
                    .cancellable(id: state.mutationID)
            case .resolveConflictsTapped:
                guard state.canAct, state.pr?.hasConflicts == true else { return .none }
                state.isSending = true
                state.mutationOutcome = nil
                let id = state.id
                let epoch = state.presentationEpoch
                return .run { send in await send(.mutationFinished(await operations.resolveConflicts(id), epoch)) }
                    .cancellable(id: state.mutationID)
            case .setAsideTapped:
                guard state.canAct else { return .none }
                state.isSending = true
                state.mutationOutcome = nil
                let id = state.id
                let epoch = state.presentationEpoch
                return .run { send in await send(.mutationFinished(await operations.setAside(id), epoch)) }
                    .cancellable(id: state.mutationID)
            case .mutationFinished(let outcome, let epoch):
                guard epoch == state.presentationEpoch, state.isSending else { return .none }
                state.isSending = false
                state.mutationOutcome = outcome
                if outcome == .confirmed {
                    state.shouldDismiss = true
                    state.successfulActionCount += 1
                } else if outcome == .closed {
                    state.isClosed = true
                }
                return .none
            case .commandResolved(let id, let rejection):
                guard state.mutationOutcome?.uncertainID == id else { return .none }
                state.isSending = true
                return .send(.mutationFinished(rejection.map(PRMutationOutcome.rejected) ?? .confirmed, state.presentationEpoch))
            case .commandReconciled(let id):
                if state.mutationOutcome?.uncertainID == id { state.mutationOutcome = nil }
                return .none
            }
        }
    }
}
