import AlondraShared
import AlondraAttachmentsFeature
import AlondraQuestionsFeature
import ComposableArchitecture
import SwiftUI

struct ServiceAttachmentDocument: View {
    let name: String
    let path: String
    let section: DocumentSection
    let store: StoreOf<DocumentReviewFeature>
    var focus: FocusState<DetailField?>.Binding
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 6) {
                Image(systemName: "doc.text").foregroundStyle(.secondary)
                Text(name).font(.callout.weight(.medium)).foregroundStyle(.secondary)
                Spacer()
                Button("Open") { store.send(.downloads(.open(path: path, name: name))) }
                    .buttonStyle(.link).font(.callout).help("Open with the default app")
                    .disabled(store.downloads.isLoading || store.downloads.isOpening)
            }.padding(.horizontal, 10)
            Divider()
            if section.isLoading {
                ProgressView().controlSize(.small).frame(maxWidth: .infinity, minHeight: 60)
            } else if let error = section.error {
                Label("Couldn’t read it: \(error)", systemImage: "exclamationmark.triangle")
                    .foregroundStyle(.secondary).padding(.horizontal, 10)
            } else {
                ServiceReviewableDocument(text: section.text, anchorPrefix: path, store: store, focus: focus)
            }
        }
    }
}

struct ServiceLinksRow: View {
    let links: [LinkRef]
    let attachments: [Attachment]
    let store: StoreOf<AttachmentDownloadFeature>
    var body: some View {
        if !links.isEmpty || !attachments.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(links, id: \.self) { link in
                    if let url = URL(string: link.url) {
                        Link(destination: url) { Label(link.title ?? link.url, systemImage: "arrow.up.right.square").lineLimit(1) }.help(link.url)
                    }
                }
                ForEach(attachments, id: \.self) { file in
                    Button {
                        if let path = file.storedPath { store.send(.open(path: path, name: file.name)) }
                    } label: { Label(file.name, systemImage: "paperclip").lineLimit(1) }
                        .buttonStyle(.link).help(file.storedPath ?? file.sourcePath)
                        .disabled(file.storedPath == nil || store.isLoading || store.isOpening)
                }
            }.font(.callout)
        }
    }
}
