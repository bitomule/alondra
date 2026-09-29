import AlondraShared
import AlondraAttachmentsFeature
import AlondraQuestionsFeature
import ComposableArchitecture
import SwiftUI

struct ServiceAttachmentDocument: View {
    let name: String
    let anchor: String
    /// A file the Mac can open; a document revision has none.
    var attachment: DomainV2.AttachmentRef?
    let section: DocumentSection
    let store: StoreOf<DocumentReviewFeature>
    var focus: FocusState<DetailField?>.Binding
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 6) {
                Image(systemName: "doc.text").foregroundStyle(.secondary)
                Text(name).font(.callout.weight(.medium)).foregroundStyle(.secondary)
                Spacer()
                if let attachment {
                    Button("Open") { store.send(.downloads(.open(attachment))) }
                        .buttonStyle(.link).font(.callout).help("Open with the default app")
                        .disabled(store.downloads.isLoading || store.downloads.isOpening)
                }
            }.padding(.horizontal, 10)
            Divider()
            if section.isLoading {
                ProgressView().controlSize(.small).frame(maxWidth: .infinity, minHeight: 60)
            } else if let error = section.error {
                Label("Couldn’t read it: \(error)", systemImage: "exclamationmark.triangle")
                    .foregroundStyle(.secondary).padding(.horizontal, 10)
            } else {
                ServiceReviewableDocument(text: section.text, anchorPrefix: anchor, store: store, focus: focus)
            }
        }
    }
}

struct ServiceLinksRow: View {
    let attachments: [DomainV2.AttachmentRef]
    let store: StoreOf<AttachmentDownloadFeature>
    var body: some View {
        if !attachments.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(attachments, id: \.id) { file in
                    Button {
                        store.send(.open(file))
                    } label: { Label(file.name, systemImage: "paperclip").lineLimit(1) }
                        .buttonStyle(.link).help(file.name)
                        .disabled(store.isLoading || store.isOpening)
                }
            }.font(.callout)
        }
    }
}
