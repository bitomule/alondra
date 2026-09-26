import AlondraShared
import ComposableArchitecture
import CoreTransferable
import QuickLook
import SwiftUI
import UniformTypeIdentifiers

/// The system copies the leased file; no duplicate file bytes are held in view state.
private struct AttachmentExport: Transferable {
    let url: URL
    static var transferRepresentation: some TransferRepresentation {
        FileRepresentation(exportedContentType: .data) { item in
            SentTransferredFile(item.url)
        }
    }
}

public struct AttachmentDownloads: View {
    let store: StoreOf<AttachmentDownloadFeature>
    let attachments: [Attachment]
    public init(store: StoreOf<AttachmentDownloadFeature>, attachments: [Attachment]) {
        self.store = store; self.attachments = attachments
    }
    public var body: some View {
        ForEach(Array(attachments.enumerated()), id: \.offset) { _, attachment in
            if let path = attachment.storedPath {
                Button { store.send(.download(path: path, name: attachment.name)) } label: {
                    Label(attachment.name, systemImage: "arrow.down.doc")
                }
                .disabled(store.exporting || store.previewURL != nil)
            } else {
                Label(attachment.name + " (unavailable)", systemImage: "doc.badge.ellipsis")
                    .foregroundStyle(.secondary)
            }
        }
    }
}

public struct AttachmentDownloadControls: View {
    let store: StoreOf<AttachmentDownloadFeature>
    public init(store: StoreOf<AttachmentDownloadFeature>) { self.store = store }
    public var body: some View {
        if store.isLoading {
            HStack { ProgressView("Downloading " + store.name); Button("Cancel") { store.send(.cancel) } }
        }
        if let error = store.error { Text(error).foregroundStyle(.red) }
        if let file = store.file {
            HStack {
                Text(file.name).lineLimit(1)
                Button("Open") { store.send(.preview) }
                Button("Export…") { store.send(.export) }
            }
        }
    }
}

public extension View {
    func attachmentDownloadPresentation(store: StoreOf<AttachmentDownloadFeature>) -> some View {
        quickLookPreview(Binding(get: { store.previewURL }, set: { if $0 == nil { store.send(.previewDismissed) } }))
            .fileExporter(
                isPresented: Binding(get: { store.exporting }, set: { _ in }),
                item: store.file.map { AttachmentExport(url: $0.url) },
                contentTypes: [.data], defaultFilename: store.file?.name,
                onCompletion: { result in
                    switch result {
                    case .success: store.send(.exportFinished(nil))
                    case .failure(let error): store.send(.exportFinished(error.localizedDescription))
                    }
                }, onCancellation: { store.send(.exportFinished(nil)) }
            )
            .onAppear { store.send(.appeared) }
            .onDisappear { store.send(.disappeared) }
    }
}
