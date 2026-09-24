import AlondraShared
import SwiftUI

struct TaskLink: Hashable {
    let id: String
}

struct TasksTab: View {
    @Environment(FleetStore.self) private var fleet
    @State private var showSettings = false
    @State private var showArchived = false

    var body: some View {
        NavigationStack {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    header
                    if fleet.overview == nil {
                        ProgressView()
                            .controlSize(.large)
                            .tint(Palette.dawn)
                            .frame(maxWidth: .infinity)
                            .padding(.top, 120)
                    } else if fleet.board.trees.isEmpty {
                        empty
                    } else {
                        ForEach(fleet.board.openTrees) { tree in
                            RequestSection(tree: tree)
                        }
                        let done = fleet.board.doneTrees
                        if !done.isEmpty {
                            Text(String(localized: "requests-done.header", defaultValue: "Done"))
                                .font(.footnote.weight(.bold))
                                .textCase(.uppercase)
                                .kerning(0.6)
                                .foregroundStyle(.secondary)
                                .padding(.horizontal, 20)
                                .padding(.top, 26)
                                .padding(.bottom, 8)
                            VStack(spacing: 0) {
                                ForEach(Array(done.enumerated()), id: \.element.id) { index, tree in
                                    DoneRequestRow(tree: tree, first: index == 0)
                                }
                            }
                            .background(Palette.card.opacity(0.85), in: .rect(cornerRadius: 24, style: .continuous))
                            .padding(.horizontal, 16)
                        }
                        let archived = fleet.board.archivedTrees
                        if !archived.isEmpty {
                            ArchivedSection(trees: archived, count: fleet.board.archivedCount, expanded: $showArchived)
                        }
                    }
                }
                .padding(.bottom, 24)
                .animation(.smooth, value: fleet.board.activeTrees.map(\.id))
                .animation(.smooth, value: fleet.board.archivedTrees.map(\.id))
            }
            .background(DawnBackground())
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Archive Completed", systemImage: "archivebox") {
                        Task { await fleet.archiveCompleted() }
                    }
                    .disabled(!fleet.canArchive)
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Settings", systemImage: "gearshape") { showSettings = true }
                }
            }
            .navigationDestination(for: TaskLink.self) { link in
                TaskDetail(taskID: link.id)
            }
            .refreshable { await fleet.refresh() }
            .sheet(isPresented: $showSettings) { SettingsView() }
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Tasks")
                .font(.display(.largeTitle, weight: .bold))
                .foregroundStyle(Palette.ink)
            Group {
                if case .offline(let reason) = fleet.connection {
                    Label(reason, systemImage: "wifi.slash")
                        .foregroundStyle(Palette.rose)
                } else {
                    Text(summary)
                        .contentTransition(.numericText())
                }
            }
            .font(.subheadline.weight(.medium))
            .foregroundStyle(.secondary)
            .lineLimit(1)
        }
        .padding(.horizontal, 20)
    }

    private var summary: String {
        let tasks = fleet.board.tasks.values
        var parts = [String(localized: "\(fleet.board.activeTrees.count) requests")]
        let needsYou = tasks.count(where: { $0.state == .needsYou })
        let inReview = tasks.count(where: { $0.state == .inReview })
        let running = tasks.count(where: { $0.state == .running })
        if needsYou > 0 { parts.append(String(localized: "\(needsYou) need you")) }
        if inReview > 0 { parts.append(String(localized: "\(inReview) in review")) }
        if running > 0 { parts.append(String(localized: "\(running) running")) }
        return parts.joined(separator: " · ")
    }

    private var empty: some View {
        ContentUnavailableView {
            Label("No tasks yet", systemImage: "checklist")
        } description: {
            Text("Ask for something in New: it shows up here at once, and the orchestrator splits it into tasks.")
        }
        .padding(.top, 60)
    }
}

private struct RequestSection: View {
    let tree: RequestTree

    @Environment(FleetStore.self) private var fleet

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .bottom, spacing: 10) {
                VStack(alignment: .leading, spacing: 5) {
                    HStack(spacing: 8) {
                        if let project = fleet.project(of: tree.request) {
                            Text(project)
                                .font(.caption.weight(.medium))
                                .foregroundStyle(.secondary)
                                .padding(.horizontal, 8)
                                .padding(.vertical, 3)
                                .background(Palette.hairline, in: .capsule)
                        }
                        Text(meta)
                            .font(.caption)
                            .foregroundStyle(.tertiary)
                    }
                    .lineLimit(1)
                    Text(fleet.title(of: tree.request))
                        .font(.display(.title3, weight: .bold))
                        .foregroundStyle(Palette.ink)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: 8)
                StateChip(label: tree.request.state.label, tint: tree.request.state.tint)
            }
            .padding(.horizontal, 4)
            .padding(.bottom, 9)
            VStack(spacing: 0) {
                if tree.rows.isEmpty {
                    DefiningRow(since: tree.request.createdAt)
                } else {
                    ForEach(Array(tree.rows.enumerated()), id: \.element.id) { index, row in
                        TaskTreeRowView(row: row, first: index == 0)
                    }
                }
            }
            .background(Palette.card.opacity(0.85), in: .rect(cornerRadius: 24, style: .continuous))
        }
        .padding(.horizontal, 16)
        .padding(.top, 24)
    }

    private var meta: String {
        let when = relative(tree.request.createdAt)
        guard !tree.rows.isEmpty else { return when }
        return "\(when) · \(String(localized: "\(tree.rows.count) tasks"))"
    }
}

struct StateChip: View {
    let label: String
    let tint: Color

    var body: some View {
        Text(label)
            .font(.caption.weight(.semibold))
            .foregroundStyle(tint)
            .padding(.horizontal, 9)
            .padding(.vertical, 4)
            .background(tint.opacity(0.14), in: .capsule)
            .lineLimit(1)
            .fixedSize()
            .padding(.bottom, 2)
    }
}

private struct DefiningRow: View {
    let since: Int64

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Glyph(symbol: "ellipsis", tint: Palette.slate, pulse: true)
            VStack(alignment: .leading, spacing: 3) {
                Text("The orchestrator is splitting it into tasks")
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(.secondary)
                StateLine(label: WorkRequest.State.defining.label, tint: Palette.slate, since: since)
            }
            Spacer(minLength: 0)
        }
        .padding(.leading, 16)
        .padding(.trailing, 14)
        .padding(.vertical, 12)
        .accessibilityElement(children: .combine)
    }
}

struct StateLine: View {
    let label: String
    let tint: Color
    let since: Int64

    var body: some View {
        HStack(spacing: 4) {
            Text(label)
                .foregroundStyle(tint)
            Text(verbatim: "· \(ago(since))")
                .foregroundStyle(.tertiary)
        }
        .font(.caption.weight(.semibold))
        .lineLimit(1)
    }
}

/// One task in its request's tree: an elbow ties it to the task it goes after; tasks on the same
/// level run in parallel.
private struct TaskTreeRowView: View {
    let row: TaskTreeRow
    let first: Bool

    @Environment(FleetStore.self) private var fleet

    static let indent: CGFloat = 38

    var body: some View {
        let task = row.task
        let pending = fleet.items(forTask: task).count
        let now = fleet.now(for: task)
        NavigationLink(value: TaskLink(id: task.id)) {
            HStack(spacing: 8) {
                HStack(alignment: .top, spacing: 12) {
                    Glyph(task: task)
                    VStack(alignment: .leading, spacing: 3) {
                        Text(task.title)
                            .font(.subheadline.weight(.semibold))
                            .foregroundStyle(task.state == .cancelled ? AnyShapeStyle(.secondary) : AnyShapeStyle(Palette.ink))
                            .strikethrough(task.state == .cancelled)
                            .multilineTextAlignment(.leading)
                        StateLine(label: task.state.label, tint: task.tint, since: task.stateSince)
                        if let now {
                            NowLine(text: now)
                                .padding(.top, 1)
                        }
                    }
                }
                Spacer(minLength: 0)
                Accessory(pending: pending)
            }
            .padding(.leading, 16 + CGFloat(row.depth) * Self.indent)
            .padding(.trailing, 14)
            .padding(.vertical, 12)
            .background(alignment: .topLeading) { TreeLines(row: row) }
            .overlay(alignment: .top) {
                if !first {
                    Palette.hairline
                        .frame(height: 1)
                        .padding(.leading, 62 + CGFloat(row.depth) * Self.indent)
                }
            }
            .contentShape(.rect)
        }
        .buttonStyle(PressStyle())
    }
}

struct NowLine: View {
    let text: String

    var body: some View {
        Text(text)
            .font(.subheadline)
            .foregroundStyle(.secondary)
            .lineLimit(2)
            .multilineTextAlignment(.leading)
    }
}

private struct Accessory: View {
    let pending: Int

    var body: some View {
        HStack(spacing: 8) {
            if pending > 0 {
                Text(pending, format: .number)
                    .font(.caption.weight(.bold).monospacedDigit())
                    .foregroundStyle(Palette.onAccent)
                    .frame(minWidth: 22, minHeight: 22)
                    .background(Palette.dawn, in: .capsule)
                    .accessibilityLabel(Text("\(pending) waiting for you"))
            }
            Image(systemName: "chevron.right")
                .font(.caption.weight(.bold))
                .foregroundStyle(.tertiary)
                .accessibilityHidden(true)
        }
    }
}

/// The rails of the tree: the elbow into this row, the lines of ancestors that go on below it,
/// and the rail down to its own children.
private struct TreeLines: View {
    let row: TaskTreeRow

    var body: some View {
        GeometryReader { proxy in
            let h = proxy.size.height
            let x = { (level: Int) in 16 + CGFloat(level) * TaskTreeRowView.indent + 17 }
            let glyphMid: CGFloat = 12 + 17
            Path { path in
                for (level, continues) in row.linesThrough.enumerated() where continues {
                    path.move(to: CGPoint(x: x(level), y: 0))
                    path.addLine(to: CGPoint(x: x(level), y: h))
                }
                if row.depth > 0 {
                    let rail = x(row.depth - 1)
                    let radius: CGFloat = 11
                    path.move(to: CGPoint(x: rail, y: 0))
                    path.addLine(to: CGPoint(x: rail, y: glyphMid - radius))
                    path.addQuadCurve(to: CGPoint(x: rail + radius, y: glyphMid), control: CGPoint(x: rail, y: glyphMid))
                    path.addLine(to: CGPoint(x: x(row.depth) - 17, y: glyphMid))
                    if !row.isLastSibling {
                        path.move(to: CGPoint(x: rail, y: glyphMid - radius))
                        path.addLine(to: CGPoint(x: rail, y: h))
                    }
                }
                if row.hasChildren {
                    path.move(to: CGPoint(x: x(row.depth), y: 12 + 34))
                    path.addLine(to: CGPoint(x: x(row.depth), y: h))
                }
            }
            .stroke(Palette.slate.opacity(0.38), style: StrokeStyle(lineWidth: 2, lineCap: .round))
        }
        .accessibilityHidden(true)
    }
}

private struct DoneRequestRow: View {
    let tree: RequestTree
    let first: Bool

    @Environment(FleetStore.self) private var fleet

    var body: some View {
        let last = tree.tasks.max { $0.stateSince < $1.stateSince }
        Group {
            if let last {
                NavigationLink(value: TaskLink(id: last.id)) { content(since: last.stateSince) }
                    .buttonStyle(PressStyle())
            } else {
                content(since: tree.request.createdAt)
            }
        }
        .overlay(alignment: .top) {
            if !first {
                Palette.hairline
                    .frame(height: 1)
                    .padding(.leading, 62)
            }
        }
    }

    private func content(since: Int64) -> some View {
        HStack(spacing: 8) {
            HStack(alignment: .top, spacing: 12) {
                Glyph(symbol: "checkmark", tint: Palette.sage)
                VStack(alignment: .leading, spacing: 3) {
                    Text(fleet.title(of: tree.request))
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(Palette.ink)
                        .multilineTextAlignment(.leading)
                    StateLine(label: tree.request.state.label, tint: Palette.sage, since: since)
                    NowLine(text: tree.rows.isEmpty ? String(localized: "Finished without tasks") : String(localized: "\(tree.rows.count) tasks"))
                        .padding(.top, 1)
                }
            }
            Spacer(minLength: 0)
            Accessory(pending: 0)
        }
        .padding(.leading, 16)
        .padding(.trailing, 14)
        .padding(.vertical, 12)
        .contentShape(.rect)
    }
}

/// Finished work the user archived: folded under its count until opened.
private struct ArchivedSection: View {
    let trees: [RequestTree]
    let count: Int
    @Binding var expanded: Bool

    var body: some View {
        Button {
            withAnimation(.smooth) { expanded.toggle() }
        } label: {
            HStack(spacing: 6) {
                Text("Archived (\(count))")
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
        .accessibilityHint(expanded ? Text("Hides the archived tasks") : Text("Shows the archived tasks"))
        .padding(.horizontal, 20)
        .padding(.top, 26)
        .padding(.bottom, 8)
        if expanded {
            VStack(spacing: 0) {
                ForEach(Array(trees.enumerated()), id: \.element.id) { index, tree in
                    DoneRequestRow(tree: tree, first: index == 0)
                }
            }
            .background(Palette.card.opacity(0.85), in: .rect(cornerRadius: 24, style: .continuous))
            .padding(.horizontal, 16)
        }
    }
}
