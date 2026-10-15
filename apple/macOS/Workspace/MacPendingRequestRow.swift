import AlondraContract
import AlondraDesign
import AlondraFeatures
import SwiftUI

/// One request in the Mac's queue: one the daemon already shows, or one sent from this Mac that it
/// does not show yet, with how it is going and, when it was not sent, why and what to do.
struct MacPendingRequestRow: View {
    private let title: String
    private let phase: PendingRequests.Phase?
    private let chips: [(String, Color)]
    private let failure: String?
    private let sent: Date?
    private let retry: () -> Void
    private let discard: () -> Void

    init(entry: PendingRequests.Entry, retry: @escaping () -> Void, discard: @escaping () -> Void) {
        title = entry.text
        phase = entry.phase
        chips = []
        failure = entry.phase.failure
        sent = nil
        self.retry = retry
        self.discard = discard
    }

    init(row: AlondraContract.Row) {
        title = row.title
        phase = nil
        chips = row.badges.map { ($0.text, Self.tint($0.tone)) }
        failure = nil
        sent = row.trailing?.instant
        retry = {}
        discard = {}
    }

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(alignment: .leading, spacing: 6) {
                Text(verbatim: title)
                    .font(.callout)
                    .foregroundStyle(Palette.ink)
                    .lineLimit(3)
                    .textSelection(.enabled)
                HStack(spacing: 6) {
                    if let phase { chip(Text("\(Image(systemName: phase.symbol)) \(Text(phase.label))"), tint: Self.tint(phase)) }
                    ForEach(chips, id: \.0) { label, tint in chip(Text(verbatim: label), tint: tint) }
                    if let sent {
                        Text(verbatim: relative(Int64(sent.timeIntervalSince1970 * 1000))).font(.caption).foregroundStyle(.secondary)
                    }
                }
                if let failure {
                    Text(verbatim: failure).font(.caption).foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: 8)
            if failure != nil {
                HStack(spacing: 8) {
                    Button(action: retry) { Label("Try Again", systemImage: "arrow.clockwise") }
                        .buttonStyle(.borderedProminent)
                        .tint(Palette.ember)
                    Button(action: discard) { Label("Discard", systemImage: "xmark") }
                        .buttonStyle(.bordered)
                }
                .controlSize(.small)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.card.opacity(0.8), in: .rect(cornerRadius: 12, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous)
            .strokeBorder(failure != nil ? Palette.rose.opacity(0.45) : Palette.hairline))
    }

    private func chip(_ text: Text, tint: Color) -> some View {
        text
            .font(.caption.weight(.semibold))
            .foregroundStyle(tint)
            .padding(.horizontal, 8).padding(.vertical, 3)
            .background(tint.opacity(0.14), in: .capsule)
    }

    private static func tint(_ phase: PendingRequests.Phase) -> Color {
        switch phase {
        case .failed: Palette.rose
        case .unconfirmed: Palette.dawn
        case .waiting, .sending, .applied: Palette.slate
        }
    }

    private static func tint(_ tone: Tone?) -> Color {
        switch tone {
        case nil, .neutral: Palette.slate
        case .info: Palette.sky
        case .accent, .warning: Palette.dawn
        case .success: Palette.sage
        case .critical: Palette.rose
        case .yourTurn: Palette.ember
        }
    }
}
