import AlondraContracts
import AlondraDomain
import AlondraDomainDependency
import AlondraShared
import ComposableArchitecture
import Foundation

/// Authorizes a merge of the presented head or asks the Agent to resolve conflicts. A recorded authorization is pending the agent, never "merged": only the PR state
/// the server observes from GitHub says that.
@Reducer
public struct PullRequestFeature {
    public enum Mutation: Equatable, Sendable { case authorize(DomainV2.MergeMethod), resolveConflicts }

    @ObservableState
    public struct State: Equatable {
        public let id: DomainV2.PrId
        public var card: DomainPullRequestCard?
        public var workspace: DomainWorkspace?
        public var isStale = false
        public var mergeConfirmationPresented = false
        public var selectedMergeMethod: DomainV2.MergeMethod?
        public var isSending = false
        public var mutationOutcome: PRMutationOutcome?
        /// The command ID of an attempt whose outcome is unknown; trying the same action again reuses it.
        public var commandID: String?
        public var commandMutation: Mutation?
        public var shouldDismiss = false
        public var successfulActionCount = 0
        public var presentationEpoch: UInt64 = 0
        public let mutationID = UUID()

        public init(card: DomainPullRequestCard, workspace: DomainWorkspace) {
            id = card.id
            self.card = card
            self.workspace = workspace
        }

        public var pr: DomainV2.PullRequest? { card?.pullRequest }
        public var isClosed: Bool { card?.isOpen != true }
        public var mergeMethods: [DomainV2.MergeMethod] { card?.pullRequest.allowedMethods ?? [] }

        public var canAct: Bool {
            card?.canAuthorizeMerge == true && !isStale && !isSending && mutationOutcome != .confirmed
        }

        public var canManage: Bool {
            card?.isOpen == true && !isStale && !isSending && mutationOutcome != .confirmed
        }
    }

    public enum Action {
        case cardChanged(DomainPullRequestCard?, DomainWorkspace?)
        case snapshotUnavailable
        case identityChanged
        case presentMergeConfirmation(Bool)
        case mergeMethodConfirmed(DomainV2.MergeMethod)
        case chooseMergeMethod(DomainV2.MergeMethod)
        case cancelMerge
        case confirmMerge
        case resolveConflictsTapped
        case mutationFinished(PRMutationOutcome, UInt64)
    }

    @Dependency(\.domainWorkspace) var client
    @Dependency(\.uuid) var uuid

    public init() {}

    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case let .cardChanged(card, workspace):
                guard card?.id == state.id || card == nil else { return .none }
                state.card = card
                state.workspace = workspace
                state.isStale = false
                if let method = state.selectedMergeMethod, !state.mergeMethods.contains(method) {
                    state.selectedMergeMethod = nil
                }
                return .none
            case .snapshotUnavailable:
                state.isStale = true
                return .none
            case .identityChanged:
                state.presentationEpoch &+= 1
                state.mergeConfirmationPresented = false
                state.card = nil
                state.workspace = nil
                state.isStale = false
                state.selectedMergeMethod = nil
                state.isSending = false
                state.mutationOutcome = nil
                state.commandID = nil
                state.shouldDismiss = false
                return .cancel(id: state.mutationID)
            case .presentMergeConfirmation(let presented):
                state.mergeConfirmationPresented = presented && state.canAct
                return .none
            case .mergeMethodConfirmed(let method):
                state.mergeConfirmationPresented = false
                guard state.canAct, state.mergeMethods.contains(method) else { return .none }
                state.selectedMergeMethod = method
                return .send(.confirmMerge)
            case .chooseMergeMethod(let method):
                guard state.canAct, state.mergeMethods.contains(method) else { return .none }
                state.selectedMergeMethod = method
                return .none
            case .cancelMerge:
                state.selectedMergeMethod = nil
                return .none
            case .confirmMerge:
                guard state.canAct, let method = state.selectedMergeMethod, state.mergeMethods.contains(method),
                      let presentation = state.card?.presentationID else {
                    state.selectedMergeMethod = nil
                    return .none
                }
                state.selectedMergeMethod = nil
                return perform(.authorizeMerge(presentation, method: method), .authorize(method), &state)
            case .resolveConflictsTapped:
                guard state.canManage, let card = state.card, card.hasConflicts,
                      let head = card.presentedHead ?? state.pr?.head else { return .none }
                return perform(.requestConflictResolution(card.id, head: head), .resolveConflicts, &state)
            case .mutationFinished(let outcome, let epoch):
                guard epoch == state.presentationEpoch, state.isSending else { return .none }
                state.isSending = false
                state.mutationOutcome = outcome
                if outcome != .unknown { state.commandID = nil }
                if outcome == .confirmed {
                    state.shouldDismiss = true
                    state.successfulActionCount += 1
                }
                return .none
            }
        }
    }

    private func perform(_ intent: DomainIntent, _ mutation: Mutation, _ state: inout State) -> Effect<Action> {
        guard let workspace = state.workspace else { return .none }
        if state.commandMutation != mutation { state.commandID = nil }
        let commandID = state.commandID ?? uuid().uuidString.lowercased()
        state.commandID = commandID
        state.commandMutation = mutation
        state.isSending = true
        state.mutationOutcome = nil
        let epoch = state.presentationEpoch
        return .run { send in
            let result = await client.perform(intent, workspace, [], commandID)
            await send(.mutationFinished(PRMutationOutcome(result), epoch))
        }
        .cancellable(id: state.mutationID)
    }
}

extension PRMutationOutcome {
    public init(_ result: DomainCommandResult) {
        switch result {
        case .recorded: self = .confirmed
        case .rejected(let rejection): self = .rejected(rejection.localizedMessage)
        case .uncertain: self = .unknown
        case .obsolete: self = .obsolete
        }
    }
}
