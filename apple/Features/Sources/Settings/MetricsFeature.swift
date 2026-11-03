import AlondraContract
import AlondraFeed
import ComposableArchitecture
import Foundation

/// Settings › Metrics: the daemon's report for one key, which every choice on the screen (period,
/// comparison, a model or project) replaces in place. A choice shows as made at once and the
/// figures wait, dimmed, for the daemon's view of it. A daemon without the screen answers that the
/// view is missing, and Settings shows no entry for it.
@Reducer public struct MetricsFeature: Sendable {
    @ObservableState public struct State: Equatable {
        public var key: ViewKey
        public var document: ViewDocument?
        public var offered = false
        /// The row just chosen, shown chosen in its section until the view it opens arrives.
        public var chosen: String?

        public init(key: ViewKey = .metrics) { self.key = key }

        /// The figures on screen are of another choice than the one made.
        public var loading: Bool { document.map { $0.key != key.rawValue } ?? false }

        /// A section the person picks one row of: the daemon marks the row on screen and gives it no link.
        public func isChoice(_ section: ViewSection) -> Bool {
            section.items.contains { item in if case .row(let row) = item { Self.isCurrent(row) } else { false } }
        }

        public func isChosen(_ row: AlondraContract.Row, in section: ViewSection) -> Bool {
            guard let chosen, section.items.contains(where: { item in if case .row(let row) = item { row.id == chosen } else { false } }) else {
                return Self.isCurrent(row)
            }
            return row.id == chosen
        }

        private static func isCurrent(_ row: AlondraContract.Row) -> Bool { row.link == nil && !row.badges.isEmpty }
    }

    public enum Action: Equatable {
        case task, stop
        case loaded(ViewDocument)
        case missing
        case choose(row: String, key: ViewKey)
    }

    @Dependency(\.viewsClient) var viewsClient
    private enum CancelID { case observe }
    public init() {}

    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .task:
                return observe(state.key)
            case .stop:
                return .cancel(id: CancelID.observe)
            case .loaded(let document):
                guard document.key == state.key.rawValue else { return .none }
                state.document = document
                state.offered = true
                state.chosen = nil
                return .none
            case .missing:
                guard let shown = state.document else {
                    state.offered = false
                    return .none
                }
                state.key = ViewKey(rawValue: shown.key)
                state.chosen = nil
                return observe(state.key)
            case let .choose(row, key):
                guard key != state.key else { return .none }
                state.key = key
                state.chosen = row
                return observe(key)
            }
        }
    }

    private func observe(_ key: ViewKey) -> Effect<Action> {
        .run { [viewsClient] send in
            do {
                for try await fetch in viewsClient.observe(key) {
                    if case .document(let document) = fetch { await send(.loaded(document)) }
                }
            } catch ViewsClientError.missingView {
                await send(.missing)
            } catch {}
        }
        .cancellable(id: CancelID.observe, cancelInFlight: true)
    }
}
