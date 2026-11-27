import AlondraContract
import SwiftUI

/// A diagram drawn, or a mockup as a thumbnail, with its name, its change and its buttons. A part
/// that cannot be shown says why.
struct SheetPartView: View {
    let part: SheetPart

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if let code = part.code {
                VStack(alignment: .leading, spacing: 8) {
                    MermaidBlockView(source: code)
                    SheetPartCaption(part: part)
                }
                .accessibilityElement(children: .contain)
                .accessibilityLabel(Text(part.alt))
            } else if let picture = part.picture {
                SheetMockupView(part: part, picture: picture)
            } else {
                unavailable
            }
        }
    }

    private var unavailable: some View {
        VStack(alignment: .leading, spacing: 8) {
            Label(part.unavailable ?? part.title, systemImage: "photo.badge.exclamationmark")
                .font(.subheadline).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            SheetPartCaption(part: part)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.night.opacity(0.6), in: .rect(cornerRadius: 12))
        .accessibilityElement(children: .combine)
        .accessibilityLabel(Text(part.alt))
    }
}
