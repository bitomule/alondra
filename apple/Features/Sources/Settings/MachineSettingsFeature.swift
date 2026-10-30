import AlondraContract
import AlondraFeed
import ComposableArchitecture
import Foundation

/// A setting of the connected Mac that its daemon offers in the `settings` section of this Mac's view.
public enum MachineSettingKind: String, CaseIterable, Hashable, Sendable {
    case mergeMethod = "merge_method"
    case branchUpdate = "branch_update"
    case nodeLimit = "node_limit"

    var command: String {
        switch self {
        case .mergeMethod: "set_merge_method"
        case .branchUpdate: "set_branch_update"
        case .nodeLimit: "set_limit"
        }
    }

    /// The row's second line right after a choice, before the daemon confirms it.
    func chosenDetail(_ label: String) -> String {
        switch self {
        case .mergeMethod, .branchUpdate:
            return label.components(separatedBy: " · ").first ?? label
        case .nodeLimit:
            return String(localized: "Nodes at once: \(label)")
        }
    }

    var unreachable: String {
        switch self {
        case .mergeMethod: String(localized: "Your Mac didn’t get the new merge method. Try again.")
        case .branchUpdate: String(localized: "Your Mac didn’t get the new way to update branches. Try again.")
        case .nodeLimit: String(localized: "Your Mac didn’t get the new limit. Try again.")
        }
    }

    var refused: String {
        switch self {
        case .mergeMethod: String(localized: "Your Mac didn’t take the new merge method.")
        case .branchUpdate: String(localized: "Your Mac didn’t take the new way to update branches.")
        case .nodeLimit: String(localized: "Your Mac didn’t take the new limit.")
        }
    }
}

/// One of the connected Mac's settings, as its daemon offers it in this Mac's view: what it is now
/// and the menu that changes it.
public struct MachineSetting: Equatable, Identifiable, Sendable {
    public var kind: MachineSettingKind
    public var title: String
    public var detail: String?
    public var action: WireAction
    public var dataEpoch: String

    public var id: MachineSettingKind { kind }

    init?(row: AlondraContract.Row, dataEpoch: String) {
        guard let kind = MachineSettingKind(rawValue: row.id),
              let action = row.actions.first(where: { $0.command == kind.command }) else { return nil }
        self.kind = kind
        title = row.title
        detail = row.subtitle
        self.action = action
        self.dataEpoch = dataEpoch
    }

    public static func all(in document: ViewDocument) -> [MachineSetting] {
        document.sections.filter { $0.id == "settings" }.flatMap(\.items).compactMap { item in
            if case .row(let row) = item { return MachineSetting(row: row, dataEpoch: document.dataEpoch) }
            return nil
        }
    }
}

/// Settings' rows for the connected Mac ("Merge de las PR", "Nodes at once"): a choice shows at
/// once, and comes back with the daemon's reason when it refuses it.
@Reducer public struct MachineSettingsFeature: Sendable {
    @ObservableState public struct State: Equatable {
        public var settings: [MachineSetting] = []
        public var chosen: [MachineSettingKind: String] = [:]
        public var sending: Set<MachineSettingKind> = []
        public var errors: [MachineSettingKind: String] = [:]
        public let client: ViewClient
        public let lang: String

        public init(client: ViewClient = .ios, lang: String = Self.deviceLanguage) {
            self.client = client; self.lang = lang
        }

        public static var deviceLanguage: String {
            Locale.preferredLanguages.first?.split(separator: "-").first == "es" ? "es" : "en"
        }

        public subscript(kind: MachineSettingKind) -> MachineSetting? {
            settings.first { $0.kind == kind }
        }

        /// The setting as it is after the last choice, before the daemon confirms it.
        public func detail(_ kind: MachineSettingKind) -> String? {
            chosen[kind].map(kind.chosenDetail) ?? self[kind]?.detail
        }
    }

    public enum Action {
        case task, stop
        case loaded(ViewDocument)
        case choose(MachineSettingKind, ActionOption)
        case answered(MachineSettingKind, ActionResponse)
        case failed(MachineSettingKind, String)
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
                state.settings = MachineSetting.all(in: document)
                state.chosen = state.chosen.filter { state.sending.contains($0.key) }
                return .none
            case let .choose(kind, option):
                guard let setting = state[kind], let actionID = setting.action.id, !state.sending.contains(kind) else { return .none }
                state.chosen[kind] = option.label
                state.sending.insert(kind)
                state.errors[kind] = nil
                let request = ActionRequest(commandId: uuid().uuidString.lowercased(), dataEpoch: setting.dataEpoch,
                                            client: state.client, lang: state.lang, actionId: actionID, optionId: option.id)
                return .run { [viewsClient] send in
                    do {
                        let prepared = try await viewsClient.prepare(request)
                        try Task.checkCancellation()
                        await send(.answered(kind, try await viewsClient.perform(prepared)))
                    }
                    catch { await send(.failed(kind, kind.unreachable)) }
                }
            case let .answered(kind, reply):
                state.sending.remove(kind)
                if reply.outcome != .applied {
                    state.chosen[kind] = nil
                    state.errors[kind] = reply.message ?? kind.refused
                }
                return .none
            case let .failed(kind, message):
                state.sending.remove(kind)
                state.chosen[kind] = nil
                state.errors[kind] = message
                return .none
            }
        }
    }
}
