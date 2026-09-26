import AlondraDomain
import ComposableArchitecture
import Foundation
import PhotosUI
import SwiftUI

public typealias AttachmentDraft = AlondraDomain.AttachmentDraft
public typealias AttachmentBudget = AlondraDomain.AttachmentBudget

public struct AttachmentUploadClient: Sendable {
    public var upload: @Sendable (AttachmentDraft) async throws -> String
    public var uploadWithProgress: @Sendable (AttachmentDraft, @escaping @Sendable (Double) -> Void) async throws -> String
    public init(upload: @escaping @Sendable (AttachmentDraft) async throws -> String) {
        self.upload = upload
        self.uploadWithProgress = { draft, _ in try await upload(draft) }
    }
    public init(uploadWithProgress: @escaping @Sendable (AttachmentDraft, @escaping @Sendable (Double) -> Void) async throws -> String) {
        self.uploadWithProgress = uploadWithProgress
        self.upload = { draft in try await uploadWithProgress(draft, { _ in }) }
    }
}
private enum AttachmentUploadKey: DependencyKey {
    static let liveValue = AttachmentUploadClient(upload: { _ in throw AttachmentFailure(message: "Attachment upload is unavailable") })
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
        public var progress: Double = 0
        public init(draft: AttachmentDraft, status: Status = .queued) { self.draft = draft; self.status = status }
    }
    @ObservableState
    public struct State: Equatable {
        public var instanceID = UUID()
        public var entries: [Entry] = []
        public var isImporting = false
        public var importID: UUID?
        public var importError: String?
        public var dropTargeted = false
        public var filesPresented = false
        public var photosPresented = false
        public var cameraPresented = false
        public var photos: [PhotosPickerItem] = []
        public var allReady: Bool {
            !isImporting && importError == nil && entries.allSatisfy { if case .uploaded = $0.status { true } else { false } }
        }
        public var storedIDs: [String] { entries.compactMap { if case .uploaded(let id) = $0.status { id } else { nil } } }
        public init() {}
        public static func == (lhs: Self, rhs: Self) -> Bool {
            lhs.entries == rhs.entries && lhs.isImporting == rhs.isImporting && lhs.importID == rhs.importID
                && lhs.dropTargeted == rhs.dropTargeted && lhs.photosPresented == rhs.photosPresented && lhs.cameraPresented == rhs.cameraPresented
                && lhs.importError == rhs.importError && lhs.filesPresented == rhs.filesPresented && lhs.photos == rhs.photos
        }
    }
    public enum Action: Sendable {
        case dropTargetChanged(Bool)
        case showFiles(Bool)
        case showPhotos(Bool)
        case showCamera(Bool)
        case cameraCaptured(Data)
        case filesSelected([URL])
        case photosSelected([PhotosPickerItem])
        case imported(AttachmentDraft, UUID)
        case importFinished(UUID)
        case importFailed(String)
        case batchFailed(String, UUID)
        case dismissError
        case reset
        case uploadNext
        case uploadProgress(UUID, Double)
        case uploaded(UUID, Result<String, AttachmentFailure>)
        case remove(UUID)
        case retry(UUID)
    }
    @Dependency(\.uuid) var uuid
    @Dependency(\.attachmentBudget) var budget
    @Dependency(\.attachmentUpload) var upload
    @Dependency(\.attachmentImport) var importer
    private enum CancelID: Hashable, Sendable { case importFiles(UUID), upload(UUID), ownerUploads(UUID) }
    public static func cancelEffects(instanceID: UUID) -> Effect<Action> {
        .merge(.cancel(id: CancelID.importFiles(instanceID)), .cancel(id: CancelID.ownerUploads(instanceID)))
    }
    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .dropTargetChanged(let targeted): state.dropTargeted = targeted; return .none
            case .showPhotos(let show): state.photosPresented = show; return .none
            case .showCamera(let show): state.cameraPresented = show; return .none
            case .cameraCaptured(let data):
                state.cameraPresented = false
                let ownerID = state.instanceID
                let batchID = uuid(); state.importID = batchID
                state.isImporting = true; state.importError = nil
                return .run { send in
                    do {
                        let draft = try await importer.camera(data)
                        try Task.checkCancellation()
                        await send(.imported(draft, batchID))
                        await send(.importFinished(batchID))
                    } catch { await send(.batchFailed(error.localizedDescription, batchID)) }
                }.cancellable(id: CancelID.importFiles(ownerID), cancelInFlight: true)
            case .showFiles(let show): state.filesPresented = show; return .none
            case .filesSelected(let urls):
                let ownerID = state.instanceID
                let batchID = uuid(); state.importID = batchID
                state.isImporting = true; state.importError = nil
                return .run { send in
                    do {
                        for url in urls {
                            try Task.checkCancellation()
                            let draft = try await importer.file(url)
                            try Task.checkCancellation()
                            await send(.imported(draft, batchID))
                        }
                        await send(.importFinished(batchID))
                    } catch { await send(.batchFailed(error.localizedDescription, batchID)) }
                }.cancellable(id: CancelID.importFiles(ownerID), cancelInFlight: true)
            case .photosSelected(let photos):
                guard !photos.isEmpty else { state.photos = []; return .none }
                let ownerID = state.instanceID
                let batchID = uuid(); state.importID = batchID
                state.photos = photos; state.isImporting = true; state.importError = nil
                return .run { send in
                    do {
                        for item in photos {
                            try Task.checkCancellation()
                            let draft = try await importer.photo(item)
                            try Task.checkCancellation()
                            await send(.imported(draft, batchID))
                        }
                        await send(.importFinished(batchID))
                    } catch { await send(.batchFailed(error.localizedDescription, batchID)) }
                }.cancellable(id: CancelID.importFiles(ownerID), cancelInFlight: true)
            case .imported(let draft, let batchID):
                guard state.importID == batchID else { return .none }
                guard budget.accepts(draft, retained: state.entries.map(\.draft)) else {
                    state.importID = nil
                    state.isImporting = false
                    state.photos = []
                    state.importError = "Attachment limit exceeded. Remove files or continue with only the selected attachments."
                    return .cancel(id: CancelID.importFiles(state.instanceID))
                }
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
                let ownerID = state.instanceID
                state = State()
                return Self.cancelEffects(instanceID: ownerID)
            case .dismissError: state.importError = nil; return .none
            case .uploadNext:
                guard !state.entries.contains(where: { $0.status == .uploading }),
                      let index = state.entries.firstIndex(where: { $0.status == .queued }) else { return .none }
                let draft = state.entries[index].draft
                let ownerID = state.instanceID
                state.entries[index].status = .uploading
                return .run { send in
                    let events = AsyncStream<Action>.makeStream()
                    await withTaskGroup(of: Void.self) { group in
                        group.addTask {
                            defer { events.continuation.finish() }
                            do {
                                let stored = try await upload.uploadWithProgress(draft) { fraction in
                                    events.continuation.yield(.uploadProgress(draft.id, fraction))
                                }
                                events.continuation.yield(.uploaded(draft.id, .success(stored)))
                            } catch is CancellationError { }
                            catch { events.continuation.yield(.uploaded(draft.id, .failure(AttachmentFailure(message: error.localizedDescription)))) }
                        }
                        for await event in events.stream { await send(event) }
                        group.cancelAll()
                    }
                }.cancellable(id: CancelID.upload(draft.id))
                    .cancellable(id: CancelID.ownerUploads(ownerID))
            case .uploadProgress(let id, let fraction):
                guard let index = state.entries.firstIndex(where: { $0.id == id }),
                      state.entries[index].status == .uploading else { return .none }
                state.entries[index].progress = min(1, max(0, fraction))
                return .none
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
                state.entries[index].progress = 0
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

public struct AttachmentImportClient: Sendable {
    public var file: @Sendable (URL) async throws -> AttachmentDraft
    public var photo: @Sendable (PhotosPickerItem) async throws -> AttachmentDraft
    public var camera: @Sendable (Data) async throws -> AttachmentDraft
    public init(file: @escaping @Sendable (URL) async throws -> AttachmentDraft,
                photo: @escaping @Sendable (PhotosPickerItem) async throws -> AttachmentDraft,
                camera: @escaping @Sendable (Data) async throws -> AttachmentDraft = { _ in throw AttachmentFailure(message: "Camera import is unavailable") }) {
        self.file = file; self.photo = photo; self.camera = camera
    }
}
private enum AttachmentImportKey: DependencyKey {
    static let liveValue = AttachmentImportClient(file: { _ in throw AttachmentFailure(message: "File import is unavailable") },
        photo: { _ in throw AttachmentFailure(message: "Photo import is unavailable") })
}
public extension DependencyValues {
    var attachmentImport: AttachmentImportClient {
        get { self[AttachmentImportKey.self] }
        set { self[AttachmentImportKey.self] = newValue }
    }
}

private enum AttachmentBudgetKey: DependencyKey {
    static let liveValue = AttachmentBudget()
}
public extension DependencyValues {
    var attachmentBudget: AttachmentBudget {
        get { self[AttachmentBudgetKey.self] }
        set { self[AttachmentBudgetKey.self] = newValue }
    }
}
