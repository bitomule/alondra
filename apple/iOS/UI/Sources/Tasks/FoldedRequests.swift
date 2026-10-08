import AlondraContract
import AlondraDesign
import AlondraFeatures
import SwiftUI

/// Finished or archived work: folded under its count until opened.
struct FoldedRequests: View {
    let fold: TasksBoard.Fold
    let expanded: Bool
    let loading: Bool
    let toggle: () -> Void
    let more: () -> Void
    let open: (AlondraContract.Link) -> Void

    var body: some View {
        if fold.count > 0 {
            Button {
                withAnimation(.smooth, toggle)
            } label: {
                HStack(spacing: 6) {
                    Text(verbatim: "\(fold.title) (\(fold.count))")
                    Image(systemName: "chevron.right")
                        .rotationEffect(.degrees(expanded ? 90 : 0))
                        .accessibilityHidden(true)
                    Spacer(minLength: 0)
                }
                .font(.footnote.weight(.bold))
                .textCase(.uppercase)
                .kerning(0.6)
                .foregroundStyle(.secondary)
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .accessibilityAddTraits(.isHeader)
            .accessibilityHint(expanded ? Text("Hides them") : Text("Shows them"))
            .padding(.horizontal, 20)
            .padding(.top, 26)
            .padding(.bottom, 8)
            if expanded && fold.requests.isEmpty && loading {
                ProgressView()
                    .frame(maxWidth: .infinity, minHeight: 44)
            } else if expanded {
                VStack(spacing: 0) {
                    ForEach(Array(fold.requests.enumerated()), id: \.element.id) { index, request in
                        FinishedRequestRow(request: request, first: index == 0, open: open)
                    }
                }
                .background(Palette.card.opacity(0.85), in: .rect(cornerRadius: 24, style: .continuous))
                .padding(.horizontal, 16)
                MoreButton(fold: fold, loading: loading, action: more)
            }
        }
    }
}

private struct FinishedRequestRow: View {
    let request: TasksBoard.Request
    let first: Bool
    let open: (AlondraContract.Link) -> Void

    private var cancelled: Bool { request.icon != .check && request.tasks.allSatisfy(\.struck) }

    var body: some View {
        if let link = request.link {
            Button { open(link) } label: { content(opensTask: true) }
                .buttonStyle(PressStyle())
        } else {
            content(opensTask: false)
        }
    }

    private func content(opensTask: Bool) -> some View {
        HStack(spacing: 8) {
            HStack(alignment: .top, spacing: 12) {
                Glyph(symbol: cancelled ? "xmark" : "checkmark", tint: cancelled ? Palette.slate : Palette.sage)
                VStack(alignment: .leading, spacing: 3) {
                    Text(verbatim: request.title)
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(Palette.ink)
                        .lineLimit(3)
                        .multilineTextAlignment(.leading)
                    if let state = request.state {
                        StateLine(label: state, tint: cancelled ? Palette.slate : Palette.sage, since: request.touchedAt)
                    }
                    if request.taskCount != 1 {
                        Group {
                            if request.tasks.isEmpty {
                                Text("Finished without tasks")
                            } else {
                                Text("\(request.taskCount) tasks")
                            }
                        }
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .padding(.top, 1)
                    }
                }
            }
            Spacer(minLength: 0)
            if opensTask {
                Image(systemName: "chevron.right")
                    .font(.caption.weight(.bold))
                    .foregroundStyle(.tertiary)
                    .accessibilityHidden(true)
            }
        }
        .padding(.leading, 16)
        .padding(.trailing, 14)
        .padding(.vertical, 12)
        .overlay(alignment: .top) {
            if !first { Hairline(leading: 62) }
        }
        .contentShape(.rect)
    }
}
