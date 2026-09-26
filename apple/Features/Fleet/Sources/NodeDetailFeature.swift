import AlondraDomain
import AlondraAttachmentsFeature
import AlondraShared
import ComposableArchitecture
import Foundation

@Reducer
public struct NodeDetailFeature {
    @ObservableState
    public struct State: Equatable {
        public var attachmentPicker = AttachmentFeature.State()
        public let sessionID: String
        public var entry: FleetNode?
        public var acceptedTick: UInt64?
        public var acceptedSequence: UInt64?
        public var isStale = false
        public var isVisible = false
        public var threads: [NodeActivity] = []
        public var threadsError: String?
        public var isLoadingThreads = false
        public var message = ""
        public var files: [NodeFile] = []
        public var pendingSubmission: AttachmentSubmissionIntent?
        public var isSending = false
        public var sendOutcome: NodeMutationOutcome?
        public var uncertainMessage: NodeMessage?
        public var sentCount = 0
        public var showCloseConfirmation = false
        public var isClosing = false
        public var closeOutcome: NodeMutationOutcome?
        public var shouldDismiss = false
        public var presentationEpoch: UInt64 = 0
        public let pollingID = UUID()
        public let mutationID = UUID()

        public init(sessionID: String) { self.sessionID = sessionID }
        public var node: Node? { entry?.node }
        public var tasks: [WorkTask] { entry?.tasks ?? [] }
        public var isSendInFlight: Bool { pendingSubmission != nil || isSending }
        public var canSend: Bool {
            attachmentPicker.canPrepareSubmission && pendingSubmission == nil && !isStale && node != nil && !isSending && !isClosing
                && sendOutcome?.uncertainID == nil && closeOutcome?.uncertainID == nil
                && closeOutcome != .confirmed
                && SendNodeMessageUseCase().prepare(text: message, files: files) != nil
        }
    }

    public enum Action {
        case attachmentPicker(AttachmentFeature.Action)
        case appeared
        case disappeared
        case snapshotChanged(FleetSnapshot)
        case snapshotUnavailable
        case identityChanged
        case threadsTick
        case threadsLoaded([NodeActivity]?, UInt64)
        case threadsFailed(String, UInt64)
        case messageChanged(String)
        case filesChanged([NodeFile])
        case sendTapped
        case sendFinished(NodeMessage, NodeMutationOutcome, UInt64)
        case closeRequested
        case closeCancelled
        case closeConfirmed
        case closeFinished(NodeMutationOutcome, UInt64)
        case commandResolved(UUID, rejection: String?)
        case commandReconciled(UUID)
    }

    @Dependency(\.nodeOperations) var operations
    @Dependency(\.continuousClock) var clock
    private let sendUseCase = SendNodeMessageUseCase()
    private let threadsUseCase = ResolveNodeThreadsUseCase()

    public init() {}

    public var body: some ReducerOf<Self> {
        Scope(state: \.attachmentPicker, action: \.attachmentPicker) { AttachmentFeature() }
        Reduce { state, action in
            switch action {
            case .attachmentPicker(let event):
                state.files = state.attachmentPicker.entries.map { NodeFile(id: $0.id, name: $0.draft.name, data: $0.draft.data, storedID: $0.storedID) }
                if state.pendingSubmission != nil, case .uploaded(_, .failure) = event,
                   let failure = state.attachmentPicker.failedUploadMessage {
                    state.pendingSubmission = nil
                    state.sendOutcome = .rejected(failure)
                }
                if event.invalidatesPendingSubmission { state.pendingSubmission = nil }
                guard state.pendingSubmission != nil, state.attachmentPicker.allReady else { return .none }
                state.pendingSubmission = nil
                return sendMessage(&state)
            case .appeared:
                guard !state.isVisible else { return .none }
                state.isVisible = true
                guard state.node != nil else { return .none }
                return pollThreads(state)
            case .disappeared:
                state.pendingSubmission = nil
                state.isVisible = false
                state.isLoadingThreads = false
                return .cancel(id: state.pollingID)
            case .snapshotChanged(let snapshot):
                guard state.acceptedSequence == nil || snapshot.sequence > state.acceptedSequence! else { return .none }
                let firstNode = state.entry == nil
                let tickChanged = state.acceptedTick != snapshot.tick
                state.acceptedSequence = snapshot.sequence
                state.acceptedTick = snapshot.tick
                state.isStale = false
                state.entry = snapshot.node(state.sessionID)
                if state.entry == nil {
                    state.pendingSubmission = nil
                    state.threads = []
                    state.isLoadingThreads = false
                    state.showCloseConfirmation = false
                    return .cancel(id: state.pollingID)
                }
                if state.isVisible && (firstNode || tickChanged) {
                    state.isLoadingThreads = false
                    return pollThreads(state)
                }
                return .none
            case .snapshotUnavailable:
                state.pendingSubmission = nil
                state.isStale = true
                return .none
            case .identityChanged:
                state.pendingSubmission = nil
                state.attachmentPicker = AttachmentFeature.State()
                state.presentationEpoch &+= 1
                state.entry = nil
                state.acceptedTick = nil
                state.acceptedSequence = nil
                state.isStale = false
                state.threads = []
                state.threadsError = nil
                state.isLoadingThreads = false
                state.message = ""
                state.files = []
                state.isSending = false
                state.sendOutcome = nil
                state.uncertainMessage = nil
                state.showCloseConfirmation = false
                state.isClosing = false
                state.closeOutcome = nil
                state.shouldDismiss = false
                return .merge(.cancel(id: state.pollingID), .cancel(id: state.mutationID))
            case .threadsTick:
                guard state.isVisible, state.node != nil, !state.isLoadingThreads else { return .none }
                state.isLoadingThreads = true
                let session = state.sessionID
                let epoch = state.presentationEpoch
                return .run { send in
                    do {
                        let threads = try await operations.threads(session)
                        try Task.checkCancellation()
                        await send(.threadsLoaded(threads, epoch))
                    } catch where !Task.isCancelled && !error.isCancellation {
                        await send(.threadsFailed(error.localizedDescription, epoch))
                    }
                }.cancellable(id: state.pollingID)
            case .threadsLoaded(let asked, let epoch):
                guard epoch == state.presentationEpoch, let node = state.node else { return .none }
                state.isLoadingThreads = false
                state.threadsError = nil
                state.threads = threadsUseCase.execute(asked: asked, node: node)
                return .none
            case .threadsFailed(let message, let epoch):
                guard epoch == state.presentationEpoch, let node = state.node else { return .none }
                state.isLoadingThreads = false
                state.threadsError = message
                if state.threads.isEmpty { state.threads = threadsUseCase.execute(asked: nil, node: node) }
                return .none
            case .messageChanged(let message):
                state.pendingSubmission = nil
                state.message = message
                return .none
            case .filesChanged(let files):
                state.pendingSubmission = nil
                state.files = files
                return .none
            case .sendTapped:
                guard state.canSend else { return .none }
                if !state.attachmentPicker.allReady {
                    state.pendingSubmission = .awaitingReceipts
                    state.sendOutcome = nil
                    return .send(.attachmentPicker(.prepareForSubmission))
                }
                return sendMessage(&state)
            case .sendFinished(let outgoing, let outcome, let epoch):
                guard epoch == state.presentationEpoch, state.isSending else { return .none }
                state.isSending = false
                state.sendOutcome = outcome
                state.uncertainMessage = outcome.uncertainID == nil ? nil : outgoing
                if outcome == .confirmed {
                    // The user may have changed the draft while the request was in flight.
                    if state.message.trimmingCharacters(in: .whitespacesAndNewlines) == outgoing.text,
                       state.files == outgoing.files {
                        state.message = ""
                        state.files = []
                        state.pendingSubmission = nil
                        state.attachmentPicker = AttachmentFeature.State()
                    }
                    state.sentCount += 1
                }
                return .none
            case .closeRequested:
                guard !state.isStale, state.node != nil, !state.isSending, !state.isClosing,
                      state.closeOutcome?.uncertainID == nil,
                      state.sendOutcome?.uncertainID == nil,
                      state.closeOutcome != .confirmed else { return .none }
                state.pendingSubmission = nil
                state.showCloseConfirmation = true
                return .none
            case .closeCancelled:
                state.showCloseConfirmation = false
                return .none
            case .closeConfirmed:
                guard state.showCloseConfirmation, !state.isStale, state.node != nil,
                      !state.isSending, !state.isClosing, state.closeOutcome?.uncertainID == nil,
                      state.sendOutcome?.uncertainID == nil,
                      state.closeOutcome != .confirmed else { return .none }
                state.pendingSubmission = nil
                state.showCloseConfirmation = false
                state.isClosing = true
                state.closeOutcome = nil
                let session = state.sessionID
                let epoch = state.presentationEpoch
                return .run { send in
                    await send(.closeFinished(await operations.close(session), epoch))
                }.cancellable(id: state.mutationID)
            case .closeFinished(let outcome, let epoch):
                guard epoch == state.presentationEpoch, state.isClosing else { return .none }
                state.isClosing = false
                state.closeOutcome = outcome
                state.shouldDismiss = outcome == .confirmed
                return .none
            case .commandResolved(let id, let rejection):
                let outcome = rejection.map(NodeMutationOutcome.rejected) ?? .confirmed
                if state.sendOutcome?.uncertainID == id, let outgoing = state.uncertainMessage {
                    state.isSending = true
                    return .send(.sendFinished(outgoing, outcome, state.presentationEpoch))
                }
                if state.closeOutcome?.uncertainID == id {
                    state.isClosing = true
                    return .send(.closeFinished(outcome, state.presentationEpoch))
                }
                return .none
            case .commandReconciled(let id):
                if state.sendOutcome?.uncertainID == id { state.sendOutcome = nil }
                if state.closeOutcome?.uncertainID == id { state.closeOutcome = nil }
                return .none
            }
        }.onChange(of: \.attachmentPicker.instanceID) { oldID, _ in
            Reduce { _, _ in
                AttachmentFeature.cancelEffects(instanceID: oldID).map { .attachmentPicker($0) }
            }
        }
    }

    private func sendMessage(_ state: inout State) -> Effect<Action> {
        guard state.canSend, let outgoing = sendUseCase.prepare(text: state.message, files: state.files) else { return .none }
        state.isSending = true
        state.sendOutcome = nil
        let session = state.sessionID
        let epoch = state.presentationEpoch
        return .run { send in
            await send(.sendFinished(outgoing, await operations.send(session, outgoing), epoch))
        }.cancellable(id: state.mutationID)
    }

    private func pollThreads(_ state: State) -> Effect<Action> {
        let id = state.pollingID
        return .run { send in
            await send(.threadsTick)
            while !Task.isCancelled {
                try await clock.sleep(for: .seconds(10))
                try Task.checkCancellation()
                await send(.threadsTick)
            }
        }.cancellable(id: id, cancelInFlight: true)
    }
}
