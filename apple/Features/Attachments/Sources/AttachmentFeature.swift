import AlondraShared
import ComposableArchitecture
import Foundation
import PhotosUI

public struct AttachmentDraft: Equatable, Sendable, Identifiable {
    public let id: UUID
    public let name: String
    public let data: Data
    public init(id: UUID = UUID(), name: String, data: Data) {
        self.id = id; self.name = name; self.data = data
    }
}
public struct AttachmentUploadClient: Sendable {
    public var upload: @Sendable (AttachmentDraft) async throws -> String
    public init(upload: @escaping @Sendable (AttachmentDraft) async throws -> String) { self.upload = upload }
}
private enum AttachmentUploadKey: DependencyKey {
    static let liveValue = AttachmentUploadClient(upload: { _ in throw AttachmentImportError.unavailable })
}
public extension DependencyValues {
    var attachmentUpload: AttachmentUploadClient {
        get { self[AttachmentUploadKey.self] }
        set { self[AttachmentUploadKey.self] = newValue }
    }
}
@Reducer
public struct AttachmentFeature {
    public init() {}
    public enum Status: Equatable, Sendable { case queued, uploading, uploaded(String), failed(String) }
    public struct Entry: Equatable, Sendable, Identifiable {
        public var id: UUID { draft.id }
        public var storedID: String? { if case .uploaded(let id) = status { id } else { nil } }
        public let draft: AttachmentDraft
        public var status: Status = .queued
        public init(draft: AttachmentDraft, status: Status = .queued) { self.draft = draft; self.status = status }
    }
    @ObservableState
    public struct State: Equatable {
        public var entries: [Entry] = []
        public var isImporting = false
        public var importID: UUID?
        public var importError: String?
        public var filesPresented = false
        public var photos: [PhotosPickerItem] = []
        public var allReady: Bool {
            !isImporting && importError == nil && entries.allSatisfy { if case .uploaded = $0.status { true } else { false } }
        }
        public var storedIDs: [String] { entries.compactMap { if case .uploaded(let id) = $0.status { id } else { nil } } }
        public init() {}
    }
    public enum Action: Sendable {
        case showFiles(Bool)
        case filesSelected([URL])
        case photosSelected([PhotosPickerItem])
        case imported(AttachmentDraft, UUID)
        case importFinished(UUID)
        case importFailed(String)
        case batchFailed(String, UUID)
        case dismissError
        case reset
        case uploadNext
        case uploaded(UUID, Result<String, AttachmentFailure>)
        case remove(UUID)
        case retry(UUID)
    }
    @Dependency(\.attachmentUpload) var upload
    private enum CancelID { case importFiles, upload(UUID) }
    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .showFiles(let show): state.filesPresented = show; return .none
            case .filesSelected(let urls):
                let batchID = UUID(); state.importID = batchID
                state.isImporting = true; state.importError = nil
                return .run { send in
                    do {
                        for url in urls { await send(.imported(try AttachmentImporter.file(url), batchID)) }
                        await send(.importFinished(batchID))
                    } catch { await send(.batchFailed(error.localizedDescription, batchID)) }
                }.cancellable(id: CancelID.importFiles, cancelInFlight: true)
            case .photosSelected(let photos):
                guard !photos.isEmpty else { state.photos = []; return .none }
                let batchID = UUID(); state.importID = batchID
                state.photos = photos; state.isImporting = true; state.importError = nil
                return .run { send in
                    do {
                        for item in photos { await send(.imported(try await AttachmentImporter.photo(item), batchID)) }
                        await send(.importFinished(batchID))
                    } catch { await send(.batchFailed(error.localizedDescription, batchID)) }
                }.cancellable(id: CancelID.importFiles, cancelInFlight: true)
            case .imported(let draft, let batchID):
                guard state.importID == batchID else { return .none }
                state.entries.append(Entry(draft: draft)); return .send(.uploadNext)
            case .importFinished(let batchID):
                guard state.importID == batchID else { return .none }
                state.importID = nil
                state.isImporting = false; state.photos = []; return .send(.uploadNext)
            case .batchFailed(let message, let batchID):
                guard state.importID == batchID else { return .none }
                state.importID = nil; state.isImporting = false; state.photos = []; state.importError = message
                return .none
            case .importFailed(let message):
                state.isImporting = false; state.photos = []; state.importError = message; return .none
            case .reset:
                let ids = state.entries.map(\.id)
                state = State()
                return .merge([.cancel(id: CancelID.importFiles)] + ids.map { .cancel(id: CancelID.upload($0)) })
            case .dismissError: state.importError = nil; return .none
            case .uploadNext:
                guard !state.entries.contains(where: { $0.status == .uploading }),
                      let index = state.entries.firstIndex(where: { $0.status == .queued }) else { return .none }
                let draft = state.entries[index].draft
                state.entries[index].status = .uploading
                return .run { send in
                    do { await send(.uploaded(draft.id, .success(try await upload.upload(draft)))) }
                    catch is CancellationError { }
                    catch { await send(.uploaded(draft.id, .failure(AttachmentFailure(message: error.localizedDescription)))) }
                }.cancellable(id: CancelID.upload(draft.id))
            case .uploaded(let id, let result):
                guard let index = state.entries.firstIndex(where: { $0.id == id }) else { return .none }
                switch result {
                case .success(let stored): state.entries[index].status = .uploaded(stored)
                case .failure(let error): state.entries[index].status = .failed(error.message)
                }
                return .send(.uploadNext)
            case .retry(let id):
                guard let index = state.entries.firstIndex(where: { $0.id == id }),
                      case .failed = state.entries[index].status else { return .none }
                state.entries[index].status = .queued
                return .send(.uploadNext)
            case .remove(let id):
                state.entries.removeAll { $0.id == id }
                return .merge(.cancel(id: CancelID.upload(id)), .send(.uploadNext))
            }
        }
    }
}
public struct AttachmentFailure: Error, Equatable, Sendable {
    public let message: String
    public init(message: String) { self.message = message }
}
private enum AttachmentImportError: LocalizedError {
    case invalidSize, unavailable
    var errorDescription: String? {
        switch self {
        case .invalidSize: "Choose a non-empty file no larger than 20 MiB."
        case .unavailable: "The attachment could not be loaded."
        }
    }
}
private enum AttachmentImporter {
    static func file(_ url: URL) throws -> AttachmentDraft {
        let access = url.startAccessingSecurityScopedResource()
        defer { if access { url.stopAccessingSecurityScopedResource() } }
        let size = try url.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0
        guard (1...20 * 1024 * 1024).contains(size) else { throw AttachmentImportError.invalidSize }
        let handle = try FileHandle(forReadingFrom: url)
        defer { try? handle.close() }
        let bytes = try handle.read(upToCount: 20 * 1024 * 1024 + 1) ?? Data()
        guard (1...20 * 1024 * 1024).contains(bytes.count) else { throw AttachmentImportError.invalidSize }
        let prepared = AttachmentFile.prepare(bytes, name: url.lastPathComponent)
        guard prepared.data.count <= 20 * 1024 * 1024 else { throw AttachmentImportError.invalidSize }
        return AttachmentDraft(name: prepared.name, data: prepared.data)
    }
    static func photo(_ item: PhotosPickerItem) async throws -> AttachmentDraft {
        guard let bytes = try await item.loadTransferable(type: Data.self) else { throw AttachmentImportError.unavailable }
        guard (1...20 * 1024 * 1024).contains(bytes.count) else { throw AttachmentImportError.invalidSize }
        let suffix = item.supportedContentTypes.first?.preferredFilenameExtension ?? "image"
        let prepared = AttachmentFile.prepare(bytes, name: "Photo." + suffix)
        guard prepared.data.count <= 20 * 1024 * 1024 else { throw AttachmentImportError.invalidSize }
        return AttachmentDraft(name: prepared.name, data: prepared.data)
    }
}
