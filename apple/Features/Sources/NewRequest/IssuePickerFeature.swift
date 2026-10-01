import AlondraContract
import AlondraFeed
import ComposableArchitecture
import Foundation

/// The open issues a request can start from. The daemon filters them and turns a pasted issue link
/// into a row of its own, so choosing one is choosing a row.
@Reducer
public struct IssuePickerFeature: Sendable {
    public init() {}

    @ObservableState
    public struct State: Equatable {
        public var query = ""
        public var rows: [AlondraContract.Row] = []
        public var empty: Notice?
        public var loading = true
        public var error: String?
        public let selected: String
        public init(selected: String = "") { self.selected = selected }
    }

    public enum Action {
        case task
        case refresh
        case queryChanged(String)
        case loaded(ViewDocument)
        case unchanged
        case failed(String)
        case pick(String)
        case delegate(Delegate)

        @CasePathable
        public enum Delegate: Equatable, Sendable { case picked(String) }
    }

    @Dependency(\.viewsClient) var viewsClient
    @Dependency(\.continuousClock) var clock
    private enum CancelID { case load, debounce }

    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .task:
                return load(.issuePicker(query: state.query.isEmpty ? nil : state.query))
            case .refresh:
                state.loading = true
                return load(.issuePicker(refresh: true))
            case .queryChanged(let query):
                state.query = query
                state.loading = true
                return .run { [clock] send in
                    try await clock.sleep(for: .milliseconds(250))
                    await send(.task)
                }.cancellable(id: CancelID.debounce, cancelInFlight: true)
            case .loaded(let document):
                state.rows = document.sections.flatMap(\.items).compactMap { item in
                    if case .row(let row) = item { row } else { nil }
                }
                state.empty = document.empty
                state.loading = false
                state.error = nil
                return .none
            case .unchanged:
                state.loading = false
                return .none
            case .failed(let message):
                state.loading = false
                state.error = message
                return .none
            case .pick(let reference):
                return .send(.delegate(.picked(reference)))
            case .delegate:
                return .none
            }
        }
    }

    private func load(_ key: ViewKey) -> Effect<Action> {
        .run { [viewsClient] send in
            do {
                switch try await viewsClient.load(key, nil) {
                case .document(let document): await send(.loaded(document))
                case .unchanged: await send(.unchanged)
                }
            } catch is CancellationError {
                return
            } catch {
                await send(.failed(String(localized: "Couldn’t load this from your Mac. Try again.")))
            }
        }.cancellable(id: CancelID.load, cancelInFlight: true)
    }
}
