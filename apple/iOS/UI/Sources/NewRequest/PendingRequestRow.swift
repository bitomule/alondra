import AlondraDesign
import AlondraFeatures
import SwiftUI

/// A request this device sent that the daemon's queue does not show yet: how it is going, and when
/// it was not sent, why, with a way to send it again or let it go.
struct PendingRequestRow: View {
    let entry: PendingRequests.Entry
    let retry: () -> Void
    let discard: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(verbatim: entry.text)
                .font(.subheadline)
                .foregroundStyle(Palette.ink)
                .lineLimit(4)
                .multilineTextAlignment(.leading)
            HStack(spacing: 6) {
                Label {
                    Text(entry.phase.label)
                } icon: {
                    Image(systemName: entry.phase.symbol)
                }
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(tint)
                    .padding(.horizontal, 9)
                    .padding(.vertical, 4)
                    .background(tint.opacity(0.14), in: .capsule)
                    .lineLimit(1)
            }
            if let failure = entry.phase.failure {
                Text(verbatim: failure)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                HStack(spacing: 10) {
                    Button(action: retry) {
                        Label("Try Again", systemImage: "arrow.clockwise")
                            .font(.subheadline.weight(.semibold))
                    }
                    .buttonStyle(.glassProminent)
                    .buttonBorderShape(.capsule)
                    .tint(Palette.ember)
                    Button(action: discard) {
                        Label("Discard", systemImage: "xmark")
                            .font(.subheadline.weight(.semibold))
                    }
                    .buttonStyle(.glass)
                    .buttonBorderShape(.capsule)
                }
                .padding(.top, 2)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(14)
        .background(Palette.card.opacity(0.8), in: .rect(cornerRadius: 18, style: .continuous))
        .overlay {
            if entry.isFailed {
                RoundedRectangle(cornerRadius: 18, style: .continuous).strokeBorder(Palette.rose.opacity(0.45))
            }
        }
        .accessibilityElement(children: .contain)
    }

    private var tint: Color {
        switch entry.phase {
        case .failed: Palette.rose
        case .unconfirmed: Palette.dawn
        case .waiting, .sending, .applied: Palette.slate
        }
    }
}
