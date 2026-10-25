import SwiftUI

/// A pull request's changes folded into counts, with the button that opens them. Until the diff
/// arrives it shows the counts the Mac already sent; when it cannot arrive, a way to try again.
public struct DiffSummaryCard: View {
    let summary: DiffSummary?
    let pending: String?
    let failed: Bool
    let shortcut: KeyboardShortcut?
    let open: () -> Void
    let retry: () -> Void

    public init(summary: DiffSummary?, pending: String? = nil, failed: Bool = false, shortcut: KeyboardShortcut? = nil,
                open: @escaping () -> Void, retry: @escaping () -> Void = {}) {
        self.summary = summary
        self.pending = pending
        self.failed = failed
        self.shortcut = shortcut
        self.open = open
        self.retry = retry
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label {
                Text("Changes", tableName: "AlondraDesign", bundle: #bundle)
            } icon: {
                Image(systemName: "arrow.left.arrow.right")
            }
            .font(.headline)
            .foregroundStyle(Palette.ink)
            .accessibilityAddTraits(.isHeader)
            if let summary {
                counts(summary)
                if !summary.breakdown.isEmpty {
                    HStack(spacing: 4) {
                        ForEach(Array(summary.breakdown.enumerated()), id: \.offset) { index, part in
                            if index > 0 { Text(verbatim: "·") }
                            part
                        }
                    }
                    .font(.footnote).foregroundStyle(.secondary)
                    .accessibilityElement(children: .combine)
                }
            } else if failed {
                Label {
                    Text("Couldn’t load the changes.", tableName: "AlondraDesign", bundle: #bundle)
                } icon: {
                    Image(systemName: "exclamationmark.triangle")
                }
                .font(.footnote).foregroundStyle(.secondary)
            } else if let pending {
                Text(verbatim: pending).font(.subheadline.monospacedDigit()).foregroundStyle(.secondary)
            }
            if failed && summary == nil {
                button(Text("Try Again", tableName: "AlondraDesign", bundle: #bundle), systemImage: "arrow.clockwise", action: retry)
            } else {
                button(Text("View changes", tableName: "AlondraDesign", bundle: #bundle), systemImage: "chevron.right", action: open)
                    .keyboardShortcut(shortcut)
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.card.opacity(0.9), in: .rect(cornerRadius: 16, style: .continuous))
    }

    private func counts(_ summary: DiffSummary) -> some View {
        HStack(spacing: 8) {
            summary.fileCount.foregroundStyle(Palette.ink)
            Text(verbatim: "+\(summary.additions)").foregroundStyle(Palette.sage)
            Text(verbatim: "−\(summary.deletions)").foregroundStyle(Palette.rose)
            DiffStatBar(additions: summary.additions, deletions: summary.deletions)
        }
        .font(.subheadline.monospacedDigit().weight(.medium))
        .accessibilityElement(children: .combine)
    }

    private func button(_ title: Text, systemImage: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack {
                title
                Spacer(minLength: 0)
                Image(systemName: systemImage).font(.footnote.weight(.semibold)).accessibilityHidden(true)
            }
            .font(.subheadline.weight(.semibold))
            .frame(maxWidth: .infinity)
        }
        #if os(iOS)
        .buttonStyle(.glass)
        #else
        .buttonStyle(.bordered)
        .controlSize(.large)
        #endif
    }
}
