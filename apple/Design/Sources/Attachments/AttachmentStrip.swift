import SwiftUI
import AlondraAttachments
import ComposableArchitecture

/// The files attached to a message being written, as a row of chips: a thumbnail for images,
/// the name and size, the upload's progress, and a way to remove each one or retry its copy.
public struct AttachmentStrip: View {
    public let store: StoreOf<AttachmentFeature>

    public init(store: StoreOf<AttachmentFeature>) { self.store = store }

    public var body: some View {
        if !store.entries.isEmpty {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    ForEach(store.entries) { item in
                        chip(item)
                    }
                }
                .padding(.vertical, 2)
            }
            .scrollClipDisabled()
        }
    }

    private func chip(_ item: AttachmentFeature.Entry) -> some View {
        HStack(spacing: 8) {
            preview(item)
            VStack(alignment: .leading, spacing: 2) {
                Text(item.draft.name)
                    .font(.caption.weight(.medium))
                    .lineLimit(1)
                    .truncationMode(.middle)
                status(item)
            }
            .frame(maxWidth: 150, alignment: .leading)
            Button {
                store.send(.remove(item.id))
            } label: {
                Image(systemName: "xmark.circle.fill")
                    .symbolRenderingMode(.hierarchical)
                    .foregroundStyle(.secondary)
            }
            .buttonStyle(.plain)
            .accessibilityLabel(Text("Remove \(item.draft.name)"))
        }
        .padding(6)
        .background(.quaternary.opacity(0.6), in: .rect(cornerRadius: 10))
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder
    private func preview(_ item: AttachmentFeature.Entry) -> some View {
        if let thumbnail = item.thumbnail {
            Image(decorative: thumbnail, scale: 1)
                .resizable()
                .scaledToFill()
                .frame(width: 36, height: 36)
                .clipShape(.rect(cornerRadius: 6))
        } else {
            Image(systemName: "doc.fill")
                .font(.title3)
                .foregroundStyle(.secondary)
                .frame(width: 36, height: 36)
        }
    }

    @ViewBuilder
    private func status(_ item: AttachmentFeature.Entry) -> some View {
        switch item.status {
        case .queued, .uploading:
            ProgressView(value: item.progress)
                .progressViewStyle(.linear)
                .frame(width: 80)
                .accessibilityLabel(Text("Copying to Alondra"))
        case .uploaded:
            Text(ByteCountFormatter.string(fromByteCount: Int64(item.draft.data.count), countStyle: .file))
                .font(.caption2)
                .foregroundStyle(.secondary)
        case .failed:
            Button {
                store.send(.retry(item.id))
            } label: {
                Label(String(localized: "Not copied · Retry"), systemImage: "arrow.clockwise")
                    .font(.caption2)
            }
            .buttonStyle(.plain)
            .foregroundStyle(.red)
        }
    }
}
