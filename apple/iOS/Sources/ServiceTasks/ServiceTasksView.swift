import AlondraWorkspaceFeature
import AlondraShared
import AlondraTasksFeature
import ComposableArchitecture
import SwiftUI

struct ServiceTasksView: View {
    @Bindable var store: StoreOf<ServiceTasksFeature>
    var focusInbox: (String) -> Void
    var showSettings: () -> Void = {}
    private var fleet: ServiceTaskPresentation { store.presentation }

    var body: some View {
        NavigationStack {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    header
                    ServiceTaskOutcome(outcome: store.board.archiveOutcome)
                    ServiceTaskOutcome(outcome: store.withdrawOutcome)
                    Button { store.send(.showSchedules(true)) } label: {
                        HStack(spacing: 12) {
                            Image(systemName: "calendar.badge.clock")
                                .font(.title3)
                                .foregroundStyle(Palette.dawn)
                            Text("Scheduled Tasks")
                                .font(.subheadline.weight(.semibold))
                                .foregroundStyle(Palette.ink)
                            Spacer()
                            Image(systemName: "chevron.right")
                                .font(.caption.weight(.semibold))
                                .foregroundStyle(.secondary)
                        }
                        .padding(16)
                        .background(Palette.card.opacity(0.85), in: .rect(cornerRadius: 18))
                    }
                    .padding(.horizontal, 16)
                    .padding(.top, 16)
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
                            RequestSection(tree: tree, store: store)
                        }
                        let done = fleet.board.doneTrees
                        if !done.isEmpty {
                            FoldedSection(title: String(localized: "Finished (\(done.count))"), trees: done, expanded: Binding(get: { store.board.showDone }, set: { store.send(.board(.showDone($0))) }), store: store)
                        }
                        let archived = fleet.board.archivedTrees
                        if !archived.isEmpty {
                            FoldedSection(title: String(localized: "Archived (\(fleet.board.archivedCount))"), trees: archived, expanded: Binding(get: { store.board.showArchived }, set: { store.send(.board(.showArchived($0))) }), store: store)
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
                        store.send(.board(.archiveTapped))
                    }
                    .disabled(!store.board.canArchive || store.board.isArchiving || store.board.archiveOutcome?.uncertainID != nil)
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Settings", systemImage: "gearshape") { showSettings() }
                }
            }
            .navigationDestination(item: $store.scope(state: \.detail, action: \.detail)) { child in
                ServiceTaskDetailView(store: child, presentation: store.presentation, openTask: { store.send(.openTask($0)) }, focusInbox: { store.send(.focusInbox($0)) })
            }
            .navigationDestination(isPresented: Binding(get: { store.showingSchedules }, set: { store.send(.showSchedules($0)) })) { ServiceSchedulesView(store: store) }
            .refreshable { await store.send(.refresh).finish() }
        }
        .task { await store.send(.appeared).finish() }
        .onDisappear { store.send(.disappeared) }
        .onChange(of: store.focusedInboxID) { _, id in
            if let id { focusInbox(id); store.send(.inboxFocusHandled) }
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Tasks")
                .font(.display(.largeTitle, weight: .bold))
                .foregroundStyle(Palette.ink)
            Group {
                if store.overview == nil, store.board.hasSnapshot {
                    Label("Connection interrupted", systemImage: "wifi.slash")
                        .foregroundStyle(Palette.rose)
                } else {
                    Text(fleet.summary)
                        .contentTransition(.numericText())
                }
            }
            .font(.subheadline.weight(.medium))
            .foregroundStyle(.secondary)
            .lineLimit(1)
        }
        .padding(.horizontal, 20)
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

    let store: StoreOf<ServiceTasksFeature>
    private var fleet: ServiceTaskPresentation { store.presentation }

    private var single: Bool { tree.rows.count == 1 }

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
                    if !single {
                        Text(fleet.title(of: tree.request))
                            .font(.display(.title3, weight: .bold))
                            .foregroundStyle(Palette.ink)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                Spacer(minLength: 8)
                if !single {
                    StateChip(label: tree.rows.isEmpty ? tree.request.definingLabel() : tree.request.state.label, tint: tree.request.state.tint)
                }
                if tree.request.canWithdraw {
                    Menu {
                        Button("Withdraw Request", systemImage: "xmark.circle", role: .destructive) {
                            store.send(.withdrawRequested(tree.request.id))
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
            VStack(spacing: 0) {
                if tree.rows.isEmpty {
                    DefiningRow(request: tree.request)
                } else {
                    ForEach(Array(tree.rows.enumerated()), id: \.element.id) { index, row in
                        TaskTreeRowView(row: row, first: index == 0, store: store)
                    }
                }
            }
            .background(Palette.card.opacity(0.85), in: .rect(cornerRadius: 24, style: .continuous))
        }
        .padding(.horizontal, 16)
        .padding(.top, 24)
        .confirmationDialog("Withdraw this request?", isPresented: Binding(get: { store.withdrawingRequestID == tree.request.id }, set: { if !$0 { store.send(.withdrawCancelled) } }), titleVisibility: .visible) {
            Button("Withdraw Request", role: .destructive) {
                store.send(.withdrawConfirmed)
            }
        } message: {
            Text("It ends as done, without tasks, and the orchestrator can no longer define it.")
        }
    }

    private var meta: String {
        let when = relative(tree.request.createdAt)
        guard tree.rows.count > 1 else { return when }
        return "\(when) · \(String(localized: "\(tree.rows.count) tasks"))"
    }
}

private struct DefiningRow: View {
    let request: WorkRequest

    var body: some View {
        let left = request.isLeftDefining()
        HStack(alignment: .top, spacing: 12) {
            Glyph(symbol: "diamond", tint: Palette.slate, pulse: !left)
            VStack(alignment: .leading, spacing: 3) {
                Text(left ? String(localized: "The orchestrator left it without tasks") : String(localized: "The orchestrator is splitting it into tasks"))
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(.secondary)
                StateLine(label: request.definingLabel(), tint: Palette.slate, since: request.createdAt)
            }
            Spacer(minLength: 0)
        }
        .padding(.leading, 16)
        .padding(.trailing, 14)
        .padding(.vertical, 12)
        .accessibilityElement(children: .combine)
    }
}

private struct TaskTreeRowView: View {
    let row: TaskTreeRow
    let first: Bool

    let store: StoreOf<ServiceTasksFeature>
    private var fleet: ServiceTaskPresentation { store.presentation }

    static let indent: CGFloat = 38

    var body: some View {
        let task = row.task
        let pending = fleet.items(forTask: task).count
        let now = fleet.now(for: task)
        Button { store.send(.openTask(task.id)) } label: {
            HStack(spacing: 8) {
                HStack(alignment: .top, spacing: 12) {
                    Glyph(task: task)
                    VStack(alignment: .leading, spacing: 3) {
                        Text(fleet.board.rowTitle(of: task))
                            .font(.subheadline.weight(.semibold))
                            .foregroundStyle(task.state == .cancelled ? AnyShapeStyle(.secondary) : AnyShapeStyle(Palette.ink))
                            .strikethrough(task.state == .cancelled)
                            .multilineTextAlignment(.leading)
                        StateLine(label: fleet.board.stateLabel(of: task), tint: task.tint, since: task.stateSince)
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

    let store: StoreOf<ServiceTasksFeature>
    private var fleet: ServiceTaskPresentation { store.presentation }

    var body: some View {
        let last = tree.tasks.max { $0.stateSince < $1.stateSince }
        Group {
            if let last {
                Button { store.send(.openTask(last.id)) } label: { content(since: last.stateSince) }
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
        let cancelled = tree.isCancelled
        return HStack(spacing: 8) {
            HStack(alignment: .top, spacing: 12) {
                Glyph(symbol: cancelled ? "xmark" : "checkmark", tint: cancelled ? Palette.slate : Palette.sage)
                VStack(alignment: .leading, spacing: 3) {
                    Text(fleet.title(of: tree.request))
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(Palette.ink)
                        .multilineTextAlignment(.leading)
                    StateLine(label: cancelled ? WorkTask.State.cancelled.label : tree.request.state.label, tint: cancelled ? Palette.slate : Palette.sage, since: since)
                    if tree.rows.count != 1 {
                        NowLine(text: tree.rows.isEmpty ? String(localized: "Finished without tasks") : String(localized: "\(tree.rows.count) tasks"))
                            .padding(.top, 1)
                    }
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

/// Finished or archived work: folded under its count until opened.
private struct FoldedSection: View {
    let title: String
    let trees: [RequestTree]
    @Binding var expanded: Bool
    let store: StoreOf<ServiceTasksFeature>

    var body: some View {
        Button {
            withAnimation(.smooth) { expanded.toggle() }
        } label: {
            HStack(spacing: 6) {
                Text(verbatim: title)
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
        if expanded {
            VStack(spacing: 0) {
                ForEach(Array(trees.enumerated()), id: \.element.id) { index, tree in
                    DoneRequestRow(tree: tree, first: index == 0, store: store)
                }
            }
            .background(Palette.card.opacity(0.85), in: .rect(cornerRadius: 24, style: .continuous))
            .padding(.horizontal, 16)
        }
    }
}
