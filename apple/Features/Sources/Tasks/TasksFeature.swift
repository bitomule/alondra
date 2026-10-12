import AlondraContract
import AlondraFeed
import ComposableArchitecture
import Foundation

/// The Tasks tab: requests with their task trees, the finished and archived folds, archiving what
/// is done, and the way into the scheduled tasks. Withdrawing and archiving show at once and undo
/// themselves if the daemon does not apply them.
@Reducer
public struct TasksFeature: Sendable {
    public init() {}

    @ObservableState
    public struct State: Equatable {
        public var screen = ScreenFeature.State(key: .tasks)
        public var actions = ActionFeature.State.ios()
        public var expanded: Set<String> = []
        public var openChains: Set<String> = []
        public var pages: [String: TasksPage] = [:]
        public var pagesVersion: String?
        public var loadingMore: Set<String> = []
        public var pageLoads: [String: UUID] = [:]
        public var optimistic = Optimistic()
        public init() {}

        public var board: TasksBoard? {
            screen.document.map { TasksBoard(document: $0, pages: pages).hiding(optimistic) }
        }
    }

    public enum Action {
        case task
        case refresh
        case foldToggled(String)
        case chainToggled(String)
        case showMore(String)
        case moreLoaded(String, request: UUID, ViewDocument)
        case moreFailed(String, request: UUID)
        case open(AlondraContract.Link)
        case archiveCompleted
        case perform(WireAction)
        case screen(ScreenFeature.Action)
        case actions(ActionFeature.Action)
        case delegate(ScreenDelegate)
    }

    @Dependency(\.viewsClient) var viewsClient
    @Dependency(\.uuid) var uuid
    private enum CancelID: Hashable { case more(String) }

    public var body: some ReducerOf<Self> {
        Scope(state: \.screen, action: \.screen) { ScreenFeature() }
        Scope(state: \.actions, action: \.actions) { ActionFeature() }
        Reduce { state, action in
            switch action {
            case .task:
                return .send(.screen(.task))
            case .refresh:
                return .run { [viewsClient] send in
                    do {
                        guard let document = try await viewsClient.document(.tasks) else { return }
                        await send(.screen(.loaded(document)))
                    } catch is CancellationError {
                        return
                    } catch {
                        await send(.delegate(.failed(String(localized: "Couldn’t load this from your Mac. Try again."))))
                    }
                }
            case .screen(.loaded(let document)):
                state.actions.dataEpoch = document.dataEpoch
                state.optimistic.reconcile(document.version, dataEpoch: document.dataEpoch)
                guard state.pagesVersion != document.version else { return .none }
                state.pages = [:]
                state.pagesVersion = nil
                state.loadingMore = []
                state.pageLoads = [:]
                return .merge(state.expanded.sorted().map { .send(.showMore($0)) })
            case .foldToggled(let id):
                guard state.expanded.insert(id).inserted else {
                    state.expanded.remove(id)
                    return .none
                }
                guard state.pages[id] == nil else { return .none }
                return .send(.showMore(id))
            case .chainToggled(let id):
                if !state.openChains.insert(id).inserted { state.openChains.remove(id) }
                return .none
            case .showMore(let id):
                guard let board = state.board, state.loadingMore.insert(id).inserted,
                      let more = [board.active, board.finished, board.archived].first(where: { $0.id == id })?.more
                else { return .none }
                let request = uuid()
                state.pageLoads[id] = request
                return .run { [viewsClient] send in
                    do {
                        guard case .document(let page) = try await viewsClient.load(ViewKey(rawValue: more.key), nil)
                        else { return }
                        await send(.moreLoaded(id, request: request, page))
                    } catch is CancellationError {
                        return
                    } catch {
                        await send(.moreFailed(id, request: request))
                    }
                }
                .cancellable(id: CancelID.more(id))
            case .moreLoaded(let id, let request, let document):
                guard state.pageLoads[id] == request else { return .none }
                state.pageLoads[id] = nil
                state.loadingMore.remove(id)
                guard let section = document.sections.first(where: { $0.id == id }) else { return .none }
                var page = state.pages[id] ?? TasksPage(rows: [], more: nil)
                page.rows += section.items.compactMap { if case .row(let row) = $0 { row } else { nil } }
                page.more = section.more
                state.pages[id] = page
                state.pagesVersion = state.screen.document?.version
                return .none
            case .moreFailed(let id, let request):
                guard state.pageLoads[id] == request else { return .none }
                state.pageLoads[id] = nil
                state.loadingMore.remove(id)
                return .send(.delegate(.failed(String(localized: "Couldn’t load this from your Mac. Try again."))))
            case .open(let link):
                return ScreenDelegate.opening(link).map { .send(.delegate($0)) } ?? .none
            case .archiveCompleted:
                guard let archive = state.board?.archiveCompleted else { return .none }
                return .send(.perform(archive))
            case .perform(let action):
                return .concatenate(.send(.actions(.present(action))), .send(.actions(.submit)))
            case .actions(.sent(let request)):
                state.optimistic.begin(request.actionId, request, dataEpoch: state.screen.document?.dataEpoch)
                return .none
            case .actions(.settled(let request, let applied)):
                state.optimistic.settle(request, applied: applied, shown: state.screen.document?.version)
                return .none
            case .actions(.delegate(let outcome)):
                return .concatenate(.send(.actions(.dismiss)), .send(.delegate(ScreenDelegate(outcome))))
            case .screen, .actions, .delegate:
                return .none
            }
        }
    }
}
