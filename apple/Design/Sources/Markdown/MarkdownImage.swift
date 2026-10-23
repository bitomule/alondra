import SwiftUI

/// An image a document shows on its own line. Only `https` images and the copies Alondra stores load;
/// any other source, or one that fails, shows its alt text in its place.
public struct MarkdownImage: View {
    public let url: URL
    public let alt: String
    @Environment(\.storedFiles) private var storedFiles

    public init(url: URL, alt: String) {
        self.url = url
        self.alt = alt
    }

    private var label: String {
        guard alt.isEmpty else { return alt }
        return StoredImage.attachment(url) == nil ? url.lastPathComponent : String(localized: "Image", table: "AlondraDesign", bundle: #bundle)
    }

    public var body: some View {
        Group {
            if MarkdownParser.isWeb(url) {
                AsyncImage(url: url) { phase in
                    switch phase {
                    case .success(let image):
                        ViewThatFits(in: [.horizontal, .vertical]) {
                            image
                            image.resizable().scaledToFit()
                        }
                    case .failure:
                        placeholder(local: false)
                    default:
                        ProgressView()
                            .frame(maxWidth: .infinity, minHeight: 60)
                    }
                }
            } else if let attachment = StoredImage.attachment(url) {
                if let storedFiles {
                    StoredImageView(attachment: attachment, files: storedFiles) { placeholder(local: false) }
                } else {
                    placeholder(local: false)
                }
            } else {
                placeholder(local: url.scheme == nil)
            }
        }
        .frame(maxWidth: 560, maxHeight: 280, alignment: .leading)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(label))
        .accessibilityAddTraits(.isImage)
    }

    private func placeholder(local: Bool) -> some View {
        Label {
            VStack(alignment: .leading, spacing: 2) {
                Text(label)
                if local {
                    Text("Local image, not available here", tableName: "AlondraDesign", bundle: #bundle)
                        .font(.caption)
                }
            }
        } icon: {
            Image(systemName: "photo")
        }
        .font(.callout)
        .foregroundStyle(.secondary)
        .padding(10)
        .background(Palette.night.opacity(0.5), in: .rect(cornerRadius: 10))
    }
}
