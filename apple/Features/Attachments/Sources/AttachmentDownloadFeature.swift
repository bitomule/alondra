import AlondraDomain
import ComposableArchitecture
import Foundation

public struct AttachmentDownloadClient: Sendable {
    public var download: @Sendable (String, String, UUID) async throws -> DownloadedAttachment
    public var release: @Sendable (DownloadedAttachment) async -> Void
    public init(download: @escaping @Sendable (String, String, UUID) async throws -> DownloadedAttachment,
                release: @escaping @Sendable (DownloadedAttachment) async -> Void) {
        self.download = download; self.release = release
    }
}
private enum AttachmentDownloadKey: DependencyKey {
    static let liveValue = AttachmentDownloadClient(download: { _, _, _ in throw AttachmentFailure(message: "File download is unavailable") }, release: { _ in })
}
public extension DependencyValues {
    var attachmentDownload: AttachmentDownloadClient {
        get { self[AttachmentDownloadKey.self] }
        set { self[AttachmentDownloadKey.self] = newValue }
    }
}
@Reducer
public struct AttachmentDownloadFeature {
    public init() {}
    @ObservableState
    public struct State: Equatable {
        public var requestIDs: [String: UUID] = [:]
        public var currentPath: String?
        public var name = ""
        public var file: DownloadedAttachment?
        public var isLoading = false
        public var error: String?
        public var previewURL: URL?
        public var exporting = false
        public var isVisible = true
        public var generation: UInt64 = 0
        public let cancellationID = UUID()
        public init() {}
    }
    public enum Action: Sendable {
        case download(path: String, name: String)
        case finished(Result<DownloadedAttachment, AttachmentFailure>, UInt64)
        case cancel
        case preview
        case previewDismissed
        case export
        case exportFinished(String?)
        case appeared, disappeared
    }
    @Dependency(\.attachmentDownload) var client
    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .download(let path, let name):
                guard !state.exporting, state.previewURL == nil else { return .none }
                let previous = state.file
                state.file = nil; state.currentPath = path; state.name = name
                state.error = nil; state.isLoading = true; state.generation &+= 1
                let generation = state.generation
                let id = state.requestIDs[path] ?? UUID(); state.requestIDs[path] = id
                return .run { send in
                    if let previous { await client.release(previous) }
                    do {
                        let file = try await client.download(path, name, id)
                        if Task.isCancelled { await client.release(file); return }
                        await send(.finished(.success(file), generation))
                    } catch is CancellationError { }
                    catch { await send(.finished(.failure(AttachmentFailure(message: error.localizedDescription)), generation)) }
                }.cancellable(id: state.cancellationID, cancelInFlight: true)
            case .finished(let result, let generation):
                guard generation == state.generation, state.isLoading else {
                    if case .success(let file) = result { return .run { _ in await client.release(file) } }
                    return .none
                }
                state.isLoading = false
                switch result {
                case .success(let file): state.file = file
                case .failure(let error): state.error = error.message
                }
                return .none
            case .cancel:
                state.generation &+= 1; state.isLoading = false
                return .cancel(id: state.cancellationID)
            case .preview:
                state.previewURL = state.file?.url; return .none
            case .export:
                state.exporting = state.file != nil; return .none
            case .previewDismissed:
                state.previewURL = nil
                return releaseIfHidden(&state)
            case .exportFinished(let error):
                state.exporting = false; state.error = error
                return releaseIfHidden(&state)
            case .appeared: state.isVisible = true; return .none
            case .disappeared:
                state.isVisible = false; state.generation &+= 1; state.isLoading = false
                return .merge(.cancel(id: state.cancellationID), releaseIfHidden(&state))
            }
        }
    }
    private func releaseIfHidden(_ state: inout State) -> Effect<Action> {
        guard !state.isVisible, state.previewURL == nil, !state.exporting, let file = state.file else { return .none }
        state.file = nil
        return .run { _ in await client.release(file) }
    }
}
