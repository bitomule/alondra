import AlondraContract
import AlondraFeed
import ComposableArchitecture
import Foundation

/// How many nodes work at once on the connected Mac, as its daemon offers it in this Mac's view:
/// where the number comes from and the menu that changes it.
public struct NodeLimitSetting: Equatable, Sendable {
    public var title: String
    public var detail: String?
    public var action: WireAction
    public var dataEpoch: String

    public init?(document: ViewDocument) {
        let rows = document.sections.filter { $0.id == "settings" }.flatMap(\.items).compactMap { item -> AlondraContract.Row? in
            if case .row(let row) = item, row.id == "node_limit" { return row }
            return nil
        }
        guard let row = rows.first, let action = row.actions.first(where: { $0.command == "set_limit" }) else { return nil }
        title = row.title
        detail = row.subtitle
        self.action = action
        dataEpoch = document.dataEpoch
    }
}

/// Settings' "Nodes at once": the choice shows at once, and comes back with the daemon's reason
/// when it refuses it.
@Reducer public struct NodeLimitFeature: Sendable {
    @ObservableState public struct State: Equatable {
        public var setting: NodeLimitSetting?
        public var chosen: String?
        public var sending = false
        public var error: String?
        public let client: ViewClient
        public let lang: String

        public init(client: ViewClient = .ios, lang: String = Self.deviceLanguage) {
            self.client = client; self.lang = lang
        }

        public static var deviceLanguage: String {
            Locale.preferredLanguages.first?.split(separator: "-").first == "es" ? "es" : "en"
        }

        /// The limit as it is after the last choice, before the daemon confirms it.
        public var detail: String? {
            chosen.map { String(localized: "Nodes at once: \($0)") } ?? setting?.detail
        }
    }

    public enum Action {
        case task, stop
        case loaded(ViewDocument)
        case choose(ActionOption)
        case answered(ActionResponse)
        case failed(String)
    }

    @Dependency(\.viewsClient) var viewsClient
    @Dependency(\.uuid) var uuid
    private enum CancelID { case observe }
    public init() {}

    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .task:
                return .run { [viewsClient] send in
                    do {
                        for try await fetch in viewsClient.observe(.machine) {
                            if case .document(let document) = fetch { await send(.loaded(document)) }
                        }
                    } catch {}
                }
                .cancellable(id: CancelID.observe, cancelInFlight: true)
            case .stop:
                return .cancel(id: CancelID.observe)
            case .loaded(let document):
                state.setting = NodeLimitSetting(document: document)
                if !state.sending { state.chosen = nil }
                return .none
            case .choose(let option):
                guard let setting = state.setting, let actionID = setting.action.id, !state.sending else { return .none }
                state.chosen = option.label
                state.sending = true
                state.error = nil
                let request = ActionRequest(commandId: uuid().uuidString.lowercased(), dataEpoch: setting.dataEpoch,
                                            client: state.client, lang: state.lang, actionId: actionID, optionId: option.id)
                return .run { [viewsClient] send in
                    do {
                        let prepared = try await viewsClient.prepare(request)
                        try Task.checkCancellation()
                        await send(.answered(try await viewsClient.perform(prepared)))
                    }
                    catch { await send(.failed(String(localized: "Your Mac didn’t get the new limit. Try again."))) }
                }
            case .answered(let reply):
                state.sending = false
                if reply.outcome != .applied {
                    state.chosen = nil
                    state.error = reply.message ?? String(localized: "Your Mac didn’t take the new limit.")
                }
                return .none
            case .failed(let message):
                state.sending = false
                state.chosen = nil
                state.error = message
                return .none
            }
        }
    }
}
