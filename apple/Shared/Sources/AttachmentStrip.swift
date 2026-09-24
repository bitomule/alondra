import SwiftUI

/// The files attached to a message being written, as a row of chips: a thumbnail for images,
/// the name and size, the upload's progress, and a way to remove each one or retry its copy.
public struct AttachmentStrip: View {
    let attachments: Attachments
    let client: AlondraClient

    public init(_ attachments: Attachments, client: AlondraClient) {
        self.attachments = attachments
        self.client = client
    }

    public var body: some View {
        if !attachments.isEmpty {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    ForEach(attachments.items) { item in
                        chip(item)
                    }
                }
                .padding(.vertical, 2)
            }
            .scrollClipDisabled()
        }
    }

    private func chip(_ item: Attachments.Item) -> some View {
        HStack(spacing: 8) {
            preview(item)
            VStack(alignment: .leading, spacing: 2) {
                Text(item.name)
                    .font(.caption.weight(.medium))
                    .lineLimit(1)
                    .truncationMode(.middle)
                status(item)
            }
            .frame(maxWidth: 150, alignment: .leading)
            Button {
                attachments.remove(item.id)
            } label: {
                Image(systemName: "xmark.circle.fill")
                    .symbolRenderingMode(.hierarchical)
                    .foregroundStyle(.secondary)
            }
            .buttonStyle(.plain)
            .accessibilityLabel(Text("Remove \(item.name)", tableName: "AlondraShared", bundle: #bundle))
        }
        .padding(6)
        .background(.quaternary.opacity(0.6), in: .rect(cornerRadius: 10))
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder
    private func preview(_ item: Attachments.Item) -> some View {
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
    private func status(_ item: Attachments.Item) -> some View {
        switch item.state {
        case .uploading(let fraction):
            ProgressView(value: fraction)
                .progressViewStyle(.linear)
                .frame(width: 80)
                .accessibilityLabel(Text("Copying to Alondra", tableName: "AlondraShared", bundle: #bundle))
        case .stored:
            Text(item.size)
                .font(.caption2)
                .foregroundStyle(.secondary)
        case .failed:
            Button {
                attachments.retry(item.id, using: client)
            } label: {
                Label(String(localized: "Not copied · Retry", table: "AlondraShared", bundle: #bundle), systemImage: "arrow.clockwise")
                    .font(.caption2)
            }
            .buttonStyle(.plain)
            .foregroundStyle(.red)
        }
    }
}
