import AlondraContract
import AlondraFeed
import ComposableArchitecture
import Foundation

/// One command the connected Mac stopped asking about because the user chose «Allow always» on its
/// card, as its daemon lists it in this Mac's view.
public struct AllowedRule: Equatable, Identifiable, Sendable {
    public var id: String
    public var title: String
    public var detail: String?
    public var remove: WireAction

    init?(row: AlondraContract.Row) {
        guard let remove = row.actions.first(where: { $0.command == "remove_rule" }) else { return nil }
        id = row.id
        title = row.title
        detail = row.subtitle
        self.remove = remove
    }

    public static func all(in document: ViewDocument) -> [AllowedRule] {
        document.sections.filter { $0.id == "allowed_rules" }.flatMap(\.items).compactMap { item in
            if case .row(let row) = item { return AllowedRule(row: row) }
            return nil
        }
    }
}

/// Settings' «Permissions allowed always»: Remove takes the row away at once and puts it back,
/// with the Mac's reason, when the Mac could not remove it.
@Reducer public struct AllowedRulesFeature: Sendable {
    @ObservableState public struct State: Equatable {
        public var rules: [AllowedRule] = []
        public var removing: Set<String> = []
        public var dataEpoch = ""
        public var error: String?
        public let client: ViewClient
        public let lang: String

        public init(client: ViewClient = .ios, lang: String = MachineSettingsFeature.State.deviceLanguage) {
            self.client = client; self.lang = lang
        }

        public var shown: [AllowedRule] { rules.filter { !removing.contains($0.id) } }
    }

    public enum Action {
        case task, stop
        case loaded(ViewDocument)
        case remove(AllowedRule)
        case answered(String, ActionResponse)
        case failed(String, String)
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
                state.rules = AllowedRule.all(in: document)
                state.dataEpoch = document.dataEpoch
                state.removing.formIntersection(state.rules.map(\.id))
                return .none
            case .remove(let rule):
                guard let actionID = rule.remove.id, !state.removing.contains(rule.id) else { return .none }
                state.removing.insert(rule.id)
                state.error = nil
                let request = ActionRequest(commandId: uuid().uuidString.lowercased(), dataEpoch: state.dataEpoch,
                                            client: state.client, lang: state.lang, actionId: actionID)
                return .run { [viewsClient] send in
                    do {
                        let prepared = try await viewsClient.prepare(request)
                        try Task.checkCancellation()
                        await send(.answered(rule.id, try await viewsClient.perform(prepared)))
                    }
                    catch { await send(.failed(rule.id, String(localized: "Your Mac didn’t remove the permission. Try again."))) }
                }
            case .answered(let id, let reply):
                if reply.outcome != .applied {
                    state.removing.remove(id)
                    state.error = reply.message ?? String(localized: "Your Mac didn’t remove the permission.")
                }
                return .none
            case .failed(let id, let message):
                state.removing.remove(id)
                state.error = message
                return .none
            }
        }
    }
}
