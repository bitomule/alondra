import ComposableArchitecture
import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

public struct AttachmentPicker: View {
    public let store: StoreOf<AttachmentFeature>
    public init(store: StoreOf<AttachmentFeature>) { self.store = store }
    public var body: some View {
        Section("Attachments") {
            HStack {
                Button("Add files", systemImage: "paperclip") { store.send(.showFiles(true)) }
                PhotosPicker(selection: Binding(get: { store.photos }, set: { store.send(.photosSelected($0)) }), matching: .images) {
                    Label("Add photos", systemImage: "photo")
                }
            }.disabled(store.isImporting)
            if store.isImporting { ProgressView("Importing…") }
            ForEach(store.entries) { entry in
                VStack(alignment: .leading) {
                    HStack {
                        Text(entry.draft.name)
                        Spacer()
                        Button("Remove", systemImage: "xmark.circle") { store.send(.remove(entry.id)) }
                    }
                    switch entry.status {
                    case .queued: Text("Waiting to upload").font(.caption)
                    case .uploading: ProgressView("Uploading…")
                    case .uploaded: Label("Ready", systemImage: "checkmark.circle").font(.caption)
                    case .failed(let message):
                        Text(message).font(.caption).foregroundStyle(.red)
                        Button("Retry upload") { store.send(.retry(entry.id)) }
                    }
                }
            }
            if let error = store.importError {
                Text(error).foregroundStyle(.red)
                Button("Continue with selected files") { store.send(.dismissError) }
            }
        }
        .fileImporter(isPresented: Binding(get: { store.filesPresented }, set: { store.send(.showFiles($0)) }), allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
            switch result {
            case .success(let urls): store.send(.filesSelected(urls))
            case .failure(let error): store.send(.importFailed(error.localizedDescription))
            }
        }
    }
}
