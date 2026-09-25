import ComposableArchitecture
import Foundation

@Reducer
public struct SettingsFeature {
    @ObservableState
    public struct State: Equatable {
        public enum Check: Equatable, Sendable {
            case idle, checking, ok, failed(String)
        }

        public var draft: String
        public var configuredAddress: String
        public var hasExternalChange = false
        public var check: Check = .idle
        public var saved = false
        public var credentialRevision: String
        public var requestRevision: UInt64 = 0
        public var operationID = UUID()

        public var candidateURL: URL? { SettingsAddress.parse(draft) }
        public var canSave: Bool { candidateURL != nil && !saved && !hasExternalChange }
        public var canCheck: Bool { candidateURL != nil && check != .checking && !hasExternalChange }

        public init(serverAddress: String, credentialRevision: String) {
            draft = serverAddress
            configuredAddress = serverAddress
            self.credentialRevision = credentialRevision
        }
    }

    public enum Action: Equatable {
        case draftChanged(String)
        case configurationChanged(String)
        case useCurrentAddress
        case keepDraft
        case credentialChanged(String)
        case testTapped
        case checked(URL, String, UInt64, String?)
        case saveTapped
        case disappeared
    }

    @Dependency(\.settingsConnection) var connection

    public init() {}

    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .draftChanged(let draft):
                state.draft = draft
                if draft == state.configuredAddress { state.hasExternalChange = false }
                state.check = .idle
                state.requestRevision &+= 1
                return .cancel(id: state.operationID)
            case .configurationChanged(let address):
                guard !state.saved, state.configuredAddress != address else { return .none }
                let wasEditing = state.draft != state.configuredAddress
                state.configuredAddress = address
                state.check = .idle
                state.requestRevision &+= 1
                if wasEditing && state.draft != address { state.hasExternalChange = true }
                else {
                    state.draft = address
                    state.hasExternalChange = false
                }
                return .cancel(id: state.operationID)
            case .useCurrentAddress:
                guard state.hasExternalChange else { return .none }
                state.draft = state.configuredAddress
                state.hasExternalChange = false
                return .none
            case .keepDraft:
                state.hasExternalChange = false
                return .none
            case .credentialChanged(let revision):
                guard state.credentialRevision != revision else { return .none }
                state.credentialRevision = revision
                state.check = .idle
                state.requestRevision &+= 1
                return .cancel(id: state.operationID)
            case .testTapped:
                guard state.canCheck, let url = state.candidateURL else { return .none }
                state.check = .checking
                state.requestRevision &+= 1
                let revision = state.requestRevision
                let credential = state.credentialRevision
                return .run { send in
                    do {
                        try await connection.check(url)
                        await send(.checked(url, credential, revision, nil))
                    } catch is CancellationError {
                        // The replacement request owns the visible result.
                    } catch {
                        await send(.checked(url, credential, revision, error.localizedDescription))
                    }
                }
                .cancellable(id: state.operationID, cancelInFlight: true)
            case let .checked(url, credential, revision, error):
                guard state.requestRevision == revision,
                      state.credentialRevision == credential,
                      state.candidateURL == url else { return .none }
                if let error { state.check = .failed(error) }
                else { state.check = .ok }
                return .none
            case .saveTapped:
                guard state.canSave, let url = state.candidateURL else { return .none }
                state.requestRevision &+= 1
                state.check = .idle
                switch connection.save(url, state.configuredAddress) {
                case .saved:
                    state.configuredAddress = url.absoluteString
                    state.saved = true
                case .conflict(let current):
                    state.configuredAddress = current
                    state.hasExternalChange = state.draft != current
                }
                return .cancel(id: state.operationID)
            case .disappeared:
                state.requestRevision &+= 1
                return .cancel(id: state.operationID)
            }
        }
    }
}
