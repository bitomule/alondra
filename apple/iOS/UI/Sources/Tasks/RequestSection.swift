import AlondraContract
import AlondraDesign
import AlondraFeatures
import SwiftUI

/// One request: who and when, what was asked, and its tasks as Fleet-style cards. A request the
/// orchestrator is still splitting says so once, under what was asked.
struct RequestSection: View {
    let request: TasksBoard.Request
    let open: (AlondraContract.Link) -> Void
    let perform: (WireAction) -> Void
    @State private var confirmingWithdraw = false
    @Environment(\.dynamicTypeSize) private var typeSize

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            header
            ForEach(request.tasks) { line in
                TaskCard(line: line, open: open)
            }
        }
        .padding(.horizontal, 16)
        .padding(.top, 28)
        .confirmationDialog(
            request.withdraw?.confirm?.title ?? "",
            isPresented: $confirmingWithdraw,
            titleVisibility: .visible
        ) {
            if let withdraw = request.withdraw {
                Button(withdraw.confirm?.button ?? withdraw.label, role: .destructive) { perform(withdraw) }
            }
        } message: {
            Text(verbatim: request.withdraw?.confirm?.message ?? "")
        }
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 8) {
            VStack(alignment: .leading, spacing: 6) {
                meta
                if !request.isSingleTask {
                    Text(verbatim: request.title)
                        .font(.headline)
                        .foregroundStyle(Palette.ink)
                        .lineLimit(3)
                        .multilineTextAlignment(.leading)
                        .accessibilityAddTraits(.isHeader)
                }
                if request.tasks.isEmpty, let state = request.state {
                    HStack(spacing: 6) {
                        Glyph(symbol: "diamond", tint: Palette.slate, size: .tiny, pulse: true)
                        Text(verbatim: state)
                            .font(.caption.weight(.semibold))
                            .foregroundStyle(Palette.slate)
                    }
                    .accessibilityElement(children: .combine)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            if request.withdraw != nil {
                Menu {
                    Button("Withdraw Request", systemImage: "xmark.circle", role: .destructive) {
                        confirmingWithdraw = true
                    }
                } label: {
                    Label("More", systemImage: "ellipsis")
                        .labelStyle(.iconOnly)
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(.secondary)
                        .frame(minWidth: 32, minHeight: 32)
                        .contentShape(.rect)
                }
            }
        }
        .padding(.horizontal, 4)
    }

    private var meta: some View {
        let layout = typeSize.isAccessibilitySize
            ? AnyLayout(VStackLayout(alignment: .leading, spacing: 4)) : AnyLayout(HStackLayout(spacing: 6))
        return layout {
            if let project = request.project {
                Text(verbatim: project)
                    .font(.caption.weight(.medium))
                    .foregroundStyle(.secondary)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 3)
                    .background(Palette.hairline, in: .capsule)
            }
            HStack(spacing: 4) {
                if let touchedAt = request.touchedAt { RelativeTime(date: touchedAt) }
                if request.tasks.count > 1 {
                    if request.touchedAt != nil { Text(verbatim: "·") }
                    Text("\(request.tasks.count) tasks")
                }
            }
            .font(.caption)
            .foregroundStyle(.tertiary)
        }
        .lineLimit(1)
    }
}

/// One task as Fleet draws a node: its state, its title, and what its node does now or said last.
/// A task that goes after another sits indented under it.
struct TaskCard: View {
    let line: TasksBoard.Line
    let open: (AlondraContract.Link) -> Void

    @Environment(\.dynamicTypeSize) private var typeSize

    private var tint: Color { TaskStyle.tint(icon: line.icon, tone: line.tone) }

    var body: some View {
        HStack(alignment: .top, spacing: 6) {
            if line.depth > 0 {
                Image(systemName: "arrow.turn.down.right")
                    .font(.caption.weight(.bold))
                    .foregroundStyle(.tertiary)
                    .padding(.leading, CGFloat(line.depth - 1) * 20 + 6)
                    .padding(.top, 18)
                    .accessibilityHidden(true)
            }
            Button { if let link = line.link { open(link) } } label: { card }
                .buttonStyle(PressStyle())
                .accessibilityElement(children: .combine)
        }
    }

    private var card: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Glyph(symbol: TaskStyle.symbol(icon: line.icon, struck: line.struck), tint: tint,
                      size: .small, pulse: line.icon == .mission)
                if let state = line.state {
                    Text(verbatim: state).font(.caption.weight(.semibold)).foregroundStyle(tint).lineLimit(1)
                }
                Spacer(minLength: 8)
                if line.waiting > 0 { WaitingBadge(count: line.waiting) }
                Image(systemName: "chevron.right")
                    .font(.caption.weight(.bold))
                    .foregroundStyle(.tertiary)
                    .accessibilityHidden(true)
            }
            Text(verbatim: line.title)
                .font(.body.weight(.semibold))
                .foregroundStyle(line.struck ? AnyShapeStyle(.secondary) : AnyShapeStyle(Palette.ink))
                .strikethrough(line.struck)
                .lineLimit(3)
                .multilineTextAlignment(.leading)
            if line.activity != nil || line.activityAt != nil {
                let layout = typeSize.isAccessibilitySize
                    ? AnyLayout(VStackLayout(alignment: .leading, spacing: 2))
                    : AnyLayout(HStackLayout(alignment: .firstTextBaseline, spacing: 6))
                layout {
                    if let activity = line.activity {
                        Text(verbatim: activity).font(.footnote).foregroundStyle(.secondary)
                            .lineLimit(typeSize.isAccessibilitySize ? 2 : 1)
                    }
                    if !typeSize.isAccessibilitySize { Spacer(minLength: 4) }
                    if let at = line.activityAt {
                        TimeText(raw: at).font(.caption).foregroundStyle(.tertiary).fixedSize()
                    }
                }
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.card.opacity(0.85), in: .rect(cornerRadius: 22, style: .continuous))
        .contentShape(.rect)
    }
}
