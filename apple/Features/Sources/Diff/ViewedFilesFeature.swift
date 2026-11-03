import AlondraContract
import AlondraDesign
import AlondraFeed
import ComposableArchitecture
import Foundation

/// GitHub's «Viewed» boxes of a pull request's files. A tap changes the box at once; the Mac saves
/// it in GitHub in the background, trying again quietly, and only after 30 seconds without
/// success does the box go back and say so. A box refused for an old head asks for the diff again.
@Reducer
public struct ViewedFilesFeature: Sendable {
    public init() {}

    public static let patience = Duration.seconds(30)

    @ObservableState
    public struct State: Equatable {
        /// The boxes GitHub had with the diff, and those saved since; nil when the Mac sent none.
        public var saved: FileMarks?
        /// Boxes ticked or cleared here that GitHub has not confirmed yet.
        public var pending: [String: Bool] = [:]
        /// The file whose box could not be saved, until the person reads it.
        public var failed: String?

        public init(saved: FileMarks? = nil) { self.saved = saved }

        /// The boxes as the person sees them: theirs first, then GitHub's.
        public var marks: [String: FileViewed]? {
            guard var files = saved?.files else { return nil }
            for (path, viewed) in pending { files[path] = viewed ? .viewed : .unviewed }
            return files
        }

        public func isViewed(_ path: String) -> Bool { marks?[path] == .viewed }
    }

    public enum Action: Equatable {
        case toggle(String)
        case saved(head: String, path: String, FileViewed)
        case gaveUp(head: String, path: String)
        case stale(head: String)
        case dismissFailure
        case delegate(Delegate)

        public enum Delegate: Equatable {
            /// The screen shows an old head or a closed pull request: its diff is fetched again.
            case reload
        }
    }

    @Dependency(\.viewsClient) var viewsClient
    @Dependency(\.continuousClock) var clock
    private struct WriteID: Hashable { let path: String }

    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .toggle(let path):
                guard let marks = state.saved, marks.files[path] != nil else { return .none }
                let viewed = !state.isViewed(path)
                state.pending[path] = viewed
                state.failed = nil
                return save(viewed, path: path, marks: marks)
            case let .saved(head, path, viewed):
                guard state.saved?.head == head else { return .none }
                state.saved?.files[path] = viewed
                state.pending[path] = nil
                return .none
            case let .gaveUp(head, path):
                guard state.saved?.head == head else { return .none }
                state.pending[path] = nil
                state.failed = path
                return .none
            case .stale(let head):
                guard state.saved?.head == head else { return .none }
                state.pending = [:]
                return .send(.delegate(.reload))
            case .dismissFailure:
                state.failed = nil
                return .none
            case .delegate:
                return .none
            }
        }
    }

    private func save(_ viewed: Bool, path: String, marks: FileMarks) -> Effect<Action> {
        .run { [viewsClient, clock] send in
            var waited = Duration.zero
            var pause = Duration.seconds(1)
            while true {
                do {
                    let saved = try await viewsClient.setFileViewed(viewed, path, marks)
                    await send(.saved(head: marks.head, path: path, saved))
                    return
                } catch FileMarkError.stale {
                    await send(.stale(head: marks.head))
                    return
                } catch FileMarkError.refused {
                    await send(.gaveUp(head: marks.head, path: path))
                    return
                } catch is CancellationError {
                    return
                } catch {}
                guard waited < Self.patience else {
                    await send(.gaveUp(head: marks.head, path: path))
                    return
                }
                let step = min(pause, Self.patience - waited)
                try await clock.sleep(for: step)
                waited += step
                pause *= 2
            }
        }
        .cancellable(id: WriteID(path: path), cancelInFlight: true)
    }
}
