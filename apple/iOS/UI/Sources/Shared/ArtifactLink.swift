import AlondraDesign
import SwiftUI

struct ArtifactLink: View {
    let url: URL
    let tint: Color

    var body: some View {
        Link(destination: url) {
            HStack(spacing: 12) {
                Image(systemName: "safari").font(.title3).foregroundStyle(tint)
                VStack(alignment: .leading, spacing: 2) {
                    Text(Artifacts.openInBrowserLabel).font(.headline).foregroundStyle(Palette.ink)
                    Text(url.absoluteString).font(.caption).lineLimit(1).truncationMode(.middle)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Image(systemName: "arrow.up.right").font(.footnote.weight(.bold)).foregroundStyle(.tertiary)
            }
            .padding(14)
            .background(tint.opacity(0.10), in: .rect(cornerRadius: 18, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 18, style: .continuous).strokeBorder(tint.opacity(0.25)))
            .contentShape(.rect(cornerRadius: 18))
        }
        .buttonStyle(.plain)
    }
}
