import AlondraContract
import AlondraDesign
import AlondraFeatures
import SwiftUI

/// One request: who and when, its title and state, and its tasks as a tree.
struct RequestSection: View {
    let request: TasksBoard.Request
    let open: (AlondraContract.Link) -> Void
    let perform: (WireAction) -> Void
    @State private var confirmingWithdraw = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            VStack(spacing: 0) {
                if request.tasks.isEmpty {
                    DefiningRow(request: request)
                } else {
                    ForEach(Array(request.tasks.enumerated()), id: \.element.id) { index, line in
                        TaskTreeRow(line: line, first: index == 0, open: open)
                    }
                }
            }
            .background(Palette.card.opacity(0.85), in: .rect(cornerRadius: 24, style: .continuous))
        }
        .padding(.horizontal, 16)
        .padding(.top, 24)
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
        HStack(alignment: .bottom, spacing: 10) {
            VStack(alignment: .leading, spacing: 5) {
                HStack(spacing: 8) {
                    if let project = request.project {
                        Text(verbatim: project)
                            .font(.caption.weight(.medium))
                            .foregroundStyle(.secondary)
                            .padding(.horizontal, 8)
                            .padding(.vertical, 3)
                            .background(Palette.hairline, in: .capsule)
                    }
                    meta
                        .font(.caption)
                        .foregroundStyle(.tertiary)
                }
                .lineLimit(1)
                if !request.isSingleTask {
                    Text(verbatim: request.title)
                        .font(.display(.title3, weight: .bold))
                        .foregroundStyle(Palette.ink)
                        .multilineTextAlignment(.leading)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: 8)
            if !request.isSingleTask, let state = request.state {
                StateChip(label: state, tint: request.tasks.isEmpty ? Palette.slate : Palette.sky)
            }
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
        .padding(.bottom, 9)
    }

    @ViewBuilder private var meta: some View {
        HStack(spacing: 4) {
            if let touchedAt = request.touchedAt { RelativeTime(date: touchedAt) }
            if request.tasks.count > 1 {
                if request.touchedAt != nil { Text(verbatim: "·") }
                Text("\(request.tasks.count) tasks")
            }
        }
    }
}

/// A request the orchestrator has not split into tasks yet.
private struct DefiningRow: View {
    let request: TasksBoard.Request

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Glyph(symbol: "diamond", tint: Palette.slate, pulse: true)
            VStack(alignment: .leading, spacing: 3) {
                Text("The orchestrator is splitting it into tasks")
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(.secondary)
                if let state = request.state {
                    StateLine(label: state, tint: Palette.slate, since: request.touchedAt)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(.leading, 16)
        .padding(.trailing, 14)
        .padding(.vertical, 12)
        .accessibilityElement(children: .combine)
    }
}

/// One task in its request's tree: an elbow ties it to the task it goes after; tasks on the same
/// level run in parallel.
struct TaskTreeRow: View {
    let line: TasksBoard.Line
    let first: Bool
    let open: (AlondraContract.Link) -> Void

    static let indent: CGFloat = 38

    var body: some View {
        Button {
            if let link = line.link { open(link) }
        } label: {
            HStack(spacing: 8) {
                HStack(alignment: .top, spacing: 12) {
                    Glyph(symbol: TaskStyle.symbol(icon: line.icon, struck: line.struck),
                          tint: TaskStyle.tint(icon: line.icon, tone: line.tone),
                          pulse: line.icon == .mission)
                    VStack(alignment: .leading, spacing: 3) {
                        Text(verbatim: line.title)
                            .font(.subheadline.weight(.semibold))
                            .foregroundStyle(line.struck ? AnyShapeStyle(.secondary) : AnyShapeStyle(Palette.ink))
                            .strikethrough(line.struck)
                            .multilineTextAlignment(.leading)
                        if let state = line.state {
                            Text(verbatim: state)
                                .font(.caption.weight(.semibold))
                                .foregroundStyle(TaskStyle.tint(icon: line.icon, tone: line.tone))
                        }
                    }
                }
                Spacer(minLength: 0)
                HStack(spacing: 8) {
                    if line.waiting > 0 { WaitingBadge(count: line.waiting) }
                    Image(systemName: "chevron.right")
                        .font(.caption.weight(.bold))
                        .foregroundStyle(.tertiary)
                        .accessibilityHidden(true)
                }
            }
            .padding(.leading, 16 + CGFloat(line.depth) * Self.indent)
            .padding(.trailing, 14)
            .padding(.vertical, 12)
            .background(alignment: .topLeading) { TreeLines(line: line) }
            .overlay(alignment: .top) {
                if !first { Hairline(leading: 62 + CGFloat(line.depth) * Self.indent) }
            }
            .contentShape(.rect)
        }
        .buttonStyle(PressStyle())
    }
}

/// The rails of the tree: the elbow into this row, the lines of ancestors that go on below it,
/// and the rail down to its own children.
private struct TreeLines: View {
    let line: TasksBoard.Line

    var body: some View {
        GeometryReader { proxy in
            let height = proxy.size.height
            let x = { (level: Int) in 16 + CGFloat(level) * TaskTreeRow.indent + 17 }
            let glyphMid: CGFloat = 12 + 17
            Path { path in
                for level in 0..<max(0, line.depth - 1) where line.rails[level + 1] {
                    path.move(to: CGPoint(x: x(level), y: 0))
                    path.addLine(to: CGPoint(x: x(level), y: height))
                }
                if line.depth > 0 {
                    let rail = x(line.depth - 1)
                    let radius: CGFloat = 11
                    path.move(to: CGPoint(x: rail, y: 0))
                    path.addLine(to: CGPoint(x: rail, y: glyphMid - radius))
                    path.addQuadCurve(to: CGPoint(x: rail + radius, y: glyphMid), control: CGPoint(x: rail, y: glyphMid))
                    path.addLine(to: CGPoint(x: x(line.depth) - 17, y: glyphMid))
                    if !line.isLastSibling {
                        path.move(to: CGPoint(x: rail, y: glyphMid - radius))
                        path.addLine(to: CGPoint(x: rail, y: height))
                    }
                }
                if line.hasChildren {
                    path.move(to: CGPoint(x: x(line.depth), y: 12 + 34))
                    path.addLine(to: CGPoint(x: x(line.depth), y: height))
                }
            }
            .stroke(Palette.slate.opacity(0.38), style: StrokeStyle(lineWidth: 2, lineCap: .round))
        }
        .accessibilityHidden(true)
    }
}
