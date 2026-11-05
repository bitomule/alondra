import AlondraContract
import ComposableArchitecture
import Foundation

/// «This Mac's settings» for one paired Mac: its projects folder, changed at once and put back
/// with the Mac's reason when it refuses, and whether the relay is on.
@Reducer public struct MacSettingsFeature: Sendable {
    @ObservableState public struct State: Equatable, Identifiable {
        public let id: UUID
        public let macName: String
        public var settings: MacSettings?
        public var status: MacStatus?
        public var problem: ConnectionProblem?
        /// Every paired Mac can be forgotten from here; the Mac's own daemon cannot.
        public var canForget: Bool
        public var confirmForget = false
        public var chosen: MacSettings.Candidate?
        public var revision: UInt64 = 0
        public var error: String?
        @Presents public var browser: FolderBrowserFeature.State?
        @ObservationStateIgnored public var client: MacSettingsClient

        public init(id: UUID, macName: String, client: MacSettingsClient, settings: MacSettings? = nil,
                    status: MacStatus? = nil, canForget: Bool = false) {
            self.id = id; self.macName = macName; self.client = client; self.settings = settings
            self.status = status; self.canForget = canForget
        }

        public static func == (lhs: Self, rhs: Self) -> Bool {
            lhs.id == rhs.id && lhs.macName == rhs.macName && lhs.settings == rhs.settings && lhs.status == rhs.status
                && lhs.canForget == rhs.canForget && lhs.confirmForget == rhs.confirmForget && lhs.problem == rhs.problem
                && lhs.chosen == rhs.chosen && lhs.revision == rhs.revision && lhs.error == rhs.error && lhs.browser == rhs.browser
        }

        /// The folder as it is after the last choice, before the Mac confirms it.
        public var projectsRoot: String? { chosen?.path ?? settings?.projectsRoot }

        /// The Mac's candidates, plus a folder chosen elsewhere while the Mac has not answered yet.
        public var candidates: [MacSettings.Candidate] {
            let known = settings?.candidates ?? []
            guard let chosen, !known.contains(where: { $0.path == chosen.path }) else { return known }
            return known + [chosen]
        }
    }

    public enum Action {
        case task
        case loaded(MacSettings)
        case loadFailed(ConnectionProblem)
        case choose(MacSettings.Candidate)
        case saved(revision: UInt64, MacSettings)
        case refused(revision: UInt64, String)
        case other
        case pickedLocalFolder(String?)
        case browser(PresentationAction<FolderBrowserFeature.Action>)
        case statusChanged(MacStatus)
        case forgetTapped, cancelForget, confirmForget
        case delegate(Delegate)

        public enum Delegate: Equatable { case forget }
    }

    public init() {}

    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .task:
                return .merge(
                    .run { [client = state.client] send in
                        for await status in client.statuses() { await send(.statusChanged(status)) }
                    },
                    .run { [client = state.client] send in
                        do { await send(.loaded(try await client.load())) }
                        catch is CancellationError {}
                        catch { await send(.loadFailed(client.problem(error))) }
                        guard let every = client.refresh else { return }
                        while true {
                            try await Task.sleep(for: every)
                            if let settings = try? await client.load() { await send(.loaded(settings)) }
                        }
                    })
            case .loaded(let settings):
                state.settings = settings
                state.problem = nil
                return .none
            case .loadFailed(let problem):
                state.problem = problem
                return .none
            case .statusChanged(let status):
                state.status = status
                return .none
            case .forgetTapped:
                state.confirmForget = state.canForget
                return .none
            case .cancelForget:
                state.confirmForget = false
                return .none
            case .confirmForget:
                state.confirmForget = false
                return .send(.delegate(.forget))
            case .delegate:
                return .none
            case .choose(let candidate):
                guard candidate.path != state.projectsRoot else { return .none }
                state.revision += 1
                state.chosen = candidate
                state.error = nil
                let revision = state.revision
                return .run { [client = state.client] send in
                    do { await send(.saved(revision: revision, try await client.save(candidate.path))) }
                    catch { await send(.refused(revision: revision, MacSettingsClient.refusal(error))) }
                }
            case .saved(let revision, let settings):
                guard revision == state.revision else { return .none }
                state.settings = settings
                state.chosen = nil
                return .none
            case .refused(let revision, let message):
                guard revision == state.revision else { return .none }
                state.chosen = nil
                state.error = message
                return .run { [client = state.client] send in
                    if let settings = try? await client.load() { await send(.loaded(settings)) }
                }
            case .other:
                if let pick = state.client.pickLocalFolder {
                    return .run { send in await send(.pickedLocalFolder(await pick())) }
                }
                state.browser = FolderBrowserFeature.State(client: state.client)
                return .none
            case .pickedLocalFolder(let path):
                guard let path else { return .none }
                return .send(.choose(MacSettings.Candidate(path: path, repos: nil)))
            case .browser(.presented(.delegate(.chose(let candidate)))):
                state.browser = nil
                return .send(.choose(candidate))
            case .browser:
                return .none
            }
        }
        .ifLet(\.$browser, action: \.browser) { FolderBrowserFeature() }
    }
}
