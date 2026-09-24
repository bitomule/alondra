import AlondraShared
import SwiftUI

struct FleetTab: View {
    @Environment(FleetStore.self) private var fleet
    @State private var query = ""
    @State private var closing: CloseRequest?

    private var sections: [(name: String, nodes: [Node])] {
        let needle = query.trimmingCharacters(in: .whitespaces)
        return fleet.projectsByCreation.compactMap { project in
            let nodes = needle.isEmpty ? project.nodes : project.nodes.filter { matches($0, project.name, needle) }
            return nodes.isEmpty ? nil : (project.name, nodes)
        }
    }

    private func matches(_ node: Node, _ project: String, _ needle: String) -> Bool {
        let tasks = fleet.board.tasks(onNode: node.sessionId).map(\.title)
        return ([node.displayName, project, node.summary, node.cwd ?? "", node.fleetLabel] + tasks)
            .contains { $0.localizedStandardContains(needle) }
    }

    var body: some View {
        NavigationStack {
            List {
                if query.trimmingCharacters(in: .whitespaces).isEmpty, let orchestrator = fleet.orchestrator {
                    Section {
                        NavigationLink(value: orchestrator.sessionId) {
                            NodeRow(node: orchestrator, pending: fleet.pending(for: orchestrator.sessionId).count)
                        }
                        .listRowBackground(Palette.ember.opacity(0.12))
                    }
                    .listSectionSpacing(.compact)
                }
                ForEach(sections, id: \.name) { section in
                    Section {
                        ForEach(section.nodes) { node in
                            NavigationLink(value: node.sessionId) {
                                NodeRow(node: node, pending: fleet.pending(for: node.sessionId).count)
                            }
                            .listRowBackground(Palette.card.opacity(0.85))
                            .swipeActions(edge: .trailing) {
                                Button {
                                    closing = CloseRequest(session: node.sessionId, name: node.displayName, kind: .node)
                                } label: {
                                    Label("Close Node", systemImage: "xmark.bin")
                                }
                                .tint(Palette.rose)
                            }
                            .contextMenu {
                                Button(role: .destructive) {
                                    closing = CloseRequest(session: node.sessionId, name: node.displayName, kind: .node)
                                } label: {
                                    Label("Close Node", systemImage: "xmark.bin")
                                }
                            }
                        }
                    } header: {
                        ProjectHeader(name: section.name, nodes: section.nodes)
                    }
                }
            }
            .scrollContentBackground(.hidden)
            .background(DawnBackground(glow: Palette.sky))
            .overlay {
                if sections.isEmpty, fleet.overview != nil {
                    ContentUnavailableView.search(text: query)
                }
            }
            .animation(.smooth, value: sections.map(\.name))
            .navigationTitle("Fleet")
            .searchable(text: $query, prompt: "Node, project or what it’s doing")
            .navigationDestination(for: String.self) { session in
                NodeDetail(session: session)
            }
            .navigationDestination(for: TaskLink.self) { link in
                TaskDetail(taskID: link.id)
            }
            .refreshable { await fleet.refresh() }
            .closeConfirmation($closing) { request in
                Task { await fleet.close(request) }
            }
        }
    }
}

private struct ProjectHeader: View {
    let name: String
    let nodes: [Node]

    var body: some View {
        HStack(spacing: 8) {
            Text(name)
                .font(.display(.title3, weight: .bold))
                .foregroundStyle(Palette.ink)
                .textCase(nil)
            Spacer()
            ForEach([NodeOccupancy.busy, .free, .waitingMachine], id: \.self) { occupancy in
                let count = nodes.count(where: { $0.occupancy == occupancy })
                if count > 0 {
                    Label(String(count), systemImage: occupancy.symbol)
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(occupancy.tint)
                        .accessibilityLabel(Text(verbatim: "\(count) \(occupancy.label)"))
                }
            }
        }
        .padding(.top, 6)
    }
}

struct NodeRow: View {
    let node: Node
    let pending: Int

    @Environment(FleetStore.self) private var fleet

    var body: some View {
        let tasks = node.isOrchestrator ? [] : fleet.board.tasks(onNode: node.sessionId)
        HStack(alignment: .top, spacing: 12) {
            StateGlyph(node: node)
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 6) {
                    Text(node.displayName)
                        .font(.mono(.subheadline, weight: .semibold))
                        .lineLimit(1)
                        .truncationMode(.middle)
                    if node.isOrchestrator {
                        Image(systemName: "point.3.connected.trianglepath.dotted")
                            .font(.caption2.weight(.bold))
                            .foregroundStyle(Palette.ember)
                            .accessibilityLabel("Orchestrator")
                    }
                }
                HStack(spacing: 4) {
                    Text(node.fleetLabel)
                        .foregroundStyle(node.glyphTint)
                    Text(verbatim: "· \(relative(node.stateSince))")
                        .foregroundStyle(.tertiary)
                }
                .font(.caption.weight(.semibold))
                .lineLimit(1)
                if let badge = node.backgroundBadge {
                    Label {
                        Text(verbatim: badge.text)
                    } icon: {
                        Image(systemName: badge.suspect ? "exclamationmark.triangle.fill" : "square.stack.3d.up")
                    }
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(badge.suspect ? Palette.ember : Palette.slate)
                    .lineLimit(1)
                }
                if !tasks.isEmpty {
                    NodeTasks(tasks: tasks)
                        .padding(.top, 5)
                } else if !oneLine.isEmpty {
                    Text(oneLine)
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                }
            }
            Spacer(minLength: 0)
            if pending > 0 {
                Text(pending, format: .number)
                    .font(.caption.weight(.bold).monospacedDigit())
                    .foregroundStyle(Palette.onAccent)
                    .frame(minWidth: 22, minHeight: 22)
                    .background(Palette.dawn, in: .capsule)
            }
        }
        .padding(.vertical, 4)
    }

    private var oneLine: String {
        let text = node.daemonNeeds ?? node.summary
        return String(MarkdownBlock.inline(text).characters)
            .replacingOccurrences(of: "\n", with: " ")
    }
}

struct StateGlyph: View {
    let node: Node

    var body: some View {
        Glyph(symbol: node.glyphSymbol, tint: node.glyphTint, pulse: node.glyphPulses)
    }
}

/// A node's tasks under it, and for those running, what each of its threads does.
struct NodeTasks: View {
    let tasks: [WorkTask]

    @Environment(FleetStore.self) private var fleet

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(tasks) { task in
                HStack(alignment: .top, spacing: 8) {
                    Glyph(task: task, size: .tiny)
                        .padding(.top, 1)
                    VStack(alignment: .leading, spacing: 5) {
                        (Text(task.title).font(.subheadline.weight(.medium)).foregroundStyle(Palette.ink)
                            + Text(verbatim: " · \(task.state.label)").font(.caption.weight(.semibold)).foregroundStyle(task.tint))
                            .lineLimit(2)
                        if task.state == .running {
                            let threads = fleet.activity(for: task)
                            if !threads.isEmpty {
                                VStack(alignment: .leading, spacing: 5) {
                                    ForEach(Array(threads.enumerated()), id: \.offset) { _, thread in
                                        ActivityLine(activity: thread)
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

struct ActivityLine: View {
    let activity: NodeActivity

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Image(systemName: activity.isMainThread ? "terminal" : "person.2")
                .font(.caption2)
                .foregroundStyle(.secondary)
                .frame(width: 14)
                .accessibilityHidden(true)
            (Text(activity.thread).fontWeight(.semibold).foregroundStyle(Palette.ink.opacity(0.8))
                + Text(verbatim: " ")
                + Text(activity.doing ?? "").font(.mono(.caption2)).foregroundStyle(Palette.sky)
                + Text(verbatim: " · \(ago(activity.since))").foregroundStyle(.secondary))
                .font(.caption)
                .lineLimit(2)
        }
    }
}

struct NodeDetail: View {
    let session: String

    @Environment(FleetStore.self) private var fleet
    @Environment(\.dismiss) private var dismiss
    @State private var closing: CloseRequest?
    @State private var message = ""
    @State private var attachments = Attachments()
    @State private var sending = false
    @State private var sentCount = 0
    @FocusState private var focused: Bool

    var body: some View {
        Group {
            if let node = fleet.node(session) {
                content(node)
            } else {
                ContentUnavailableView("This node is gone", systemImage: "questionmark.circle")
            }
        }
        .background(DawnBackground(glow: fleet.node(session)?.glyphTint ?? Palette.dawn))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.hidden, for: .tabBar)
        .toolbar {
            if let node = fleet.node(session) {
                ToolbarItem(placement: .topBarTrailing) {
                    Menu {
                        Button(role: .destructive) {
                            closing = CloseRequest(session: node.sessionId, name: node.displayName, kind: .node)
                        } label: {
                            Label("Close Node", systemImage: "xmark.bin")
                        }
                    } label: {
                        Label("More", systemImage: "ellipsis")
                    }
                }
            }
        }
        .closeConfirmation($closing) { request in
            Task {
                if await fleet.close(request) { dismiss() }
            }
        }
        .sensoryFeedback(.success, trigger: sentCount)
    }

    private func content(_ node: Node) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 22) {
                VStack(alignment: .leading, spacing: 10) {
                    HStack(spacing: 10) {
                        StateGlyph(node: node)
                        Text(node.fleetLabel.prefix(1).uppercased() + node.fleetLabel.dropFirst())
                            .font(.subheadline.weight(.semibold))
                            .foregroundStyle(node.glyphTint)
                        Text("since \(relative(node.stateSince))")
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                    }
                    Text(node.displayName)
                        .font(.display(.largeTitle, weight: .bold))
                        .fixedSize(horizontal: false, vertical: true)
                    if let cwd = node.cwd {
                        Text(cwd.replacing(/^\/Users\/[^\/]+\//, with: "~/"))
                            .font(.mono(.caption))
                            .foregroundStyle(.secondary)
                            .lineLimit(2)
                    }
                }

                let tasks = node.isOrchestrator ? [] : fleet.board.tasks(onNode: node.sessionId)
                if !tasks.isEmpty {
                    section(String(localized: "Tasks"), symbol: "checklist") {
                        VStack(alignment: .leading, spacing: 12) {
                            ForEach(tasks) { task in
                                NavigationLink(value: TaskLink(id: task.id)) {
                                    HStack(spacing: 10) {
                                        Glyph(task: task, size: .small)
                                        VStack(alignment: .leading, spacing: 2) {
                                            Text(task.title)
                                                .font(.subheadline.weight(.semibold))
                                                .foregroundStyle(Palette.ink)
                                                .multilineTextAlignment(.leading)
                                            Text(taskLine(task))
                                                .font(.caption)
                                                .foregroundStyle(.secondary)
                                                .lineLimit(1)
                                        }
                                        Spacer(minLength: 0)
                                        Image(systemName: "chevron.right")
                                            .font(.caption.weight(.bold))
                                            .foregroundStyle(.tertiary)
                                    }
                                    .contentShape(.rect)
                                }
                                .buttonStyle(PressStyle())
                            }
                        }
                    }
                }

                let threads = fleet.activity(of: node)
                if !threads.isEmpty {
                    section(String(localized: "Right now"), symbol: "wrench.and.screwdriver") {
                        VStack(alignment: .leading, spacing: 12) {
                            ForEach(Array(threads.enumerated()), id: \.offset) { _, thread in
                                ThreadLine(activity: thread, task: thread.task.flatMap(fleet.board.task))
                            }
                        }
                    }
                }

                if !node.running.isEmpty {
                    section(BackgroundItem.sectionTitle, symbol: "square.stack.3d.up") {
                        VStack(alignment: .leading, spacing: 12) {
                            ForEach(node.running, id: \.self) { item in
                                BackgroundRow(item: item)
                            }
                        }
                    }
                }

                if let needs = node.daemonNeeds {
                    section(String(localized: "Needs"), symbol: "hand.raised") {
                        Text(needs).font(.body)
                    }
                }

                let pending = fleet.pending(for: session)
                if !pending.isEmpty {
                    section(String(localized: "Waiting for You"), symbol: "tray.full") {
                        VStack(alignment: .leading, spacing: 8) {
                            ForEach(pending) { item in
                                Button { fleet.focus(item) } label: {
                                    HStack(spacing: 10) {
                                        KindBadge(kind: Kind(item))
                                        Text(item.title)
                                            .font(.subheadline.weight(.medium))
                                            .foregroundStyle(Palette.ink)
                                            .lineLimit(2)
                                            .multilineTextAlignment(.leading)
                                        Spacer(minLength: 0)
                                        Image(systemName: "chevron.right")
                                            .font(.caption.weight(.bold))
                                            .foregroundStyle(.tertiary)
                                    }
                                    .contentShape(.rect)
                                }
                                .buttonStyle(PressStyle())
                                .accessibilityHint("Opens in Waiting for You")
                            }
                        }
                    }
                }

                if let last = node.lastMessage, !last.isEmpty {
                    section(String(localized: "What it said last"), symbol: "text.quote", trailing: node.lastMessageAt.map(relative)) {
                        VStack(alignment: .leading, spacing: 10) {
                            ForEach(MarkdownParser.segments(from: last)) { segment in
                                MarkdownSegmentView(segment: segment)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                            }
                        }
                    }
                }
            }
            .padding(20)
        }
        .scrollDismissesKeyboard(.interactively)
        .safeAreaBar(edge: .bottom) { composer(node) }
    }

    private func taskLine(_ task: WorkTask) -> String {
        guard let request = fleet.request(of: task) else { return task.state.label }
        return "\(task.state.label) · \(String(localized: "from “\(fleet.title(of: request))”"))"
    }

    private func section<Content: View>(_ title: String, symbol: String, trailing: String? = nil, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Label(title, systemImage: symbol)
                    .font(.footnote.weight(.bold))
                    .textCase(.uppercase)
                    .kerning(0.6)
                    .foregroundStyle(.secondary)
                Spacer()
                if let trailing {
                    Text(trailing).font(.caption).foregroundStyle(.tertiary)
                }
            }
            content()
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(16)
                .background(Palette.card.opacity(0.9), in: .rect(cornerRadius: 20, style: .continuous))
        }
    }

    private func composer(_ node: Node) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            AttachmentStrip(attachments, client: fleet.client)
            messageBox(node)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
    }

    private func messageBox(_ node: Node) -> some View {
        HStack(alignment: .bottom, spacing: 10) {
            HStack(alignment: .bottom, spacing: 4) {
                AttachButton(attachments: attachments)
                    .padding(.leading, 6)
                    .padding(.bottom, 4)
                TextField("Write to \(node.displayName)…", text: $message, axis: .vertical)
                    .lineLimit(1...5)
                    .focused($focused)
                    .padding(.trailing, 16)
                    .padding(.vertical, 12)
            }
            .glassEffect(.regular.interactive(), in: .rect(cornerRadius: 22))
            Button {
                send(to: node)
            } label: {
                Image(systemName: sending ? "ellipsis" : "arrow.up")
                    .font(.body.weight(.bold))
                    .frame(width: 26, height: 26)
                    .symbolEffect(.variableColor.iterative, isActive: sending)
            }
            .buttonStyle(.glassProminent)
            .buttonBorderShape(.circle)
            .tint(Palette.dawn)
            .disabled((message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && attachments.isEmpty) || sending)
            .accessibilityLabel("Send")
        }
    }

    private func send(to node: Node) {
        let text = message.trimmingCharacters(in: .whitespacesAndNewlines)
        let session = node.sessionId
        sending = true
        Task {
            let ok = await fleet.perform { [attachments] client in
                try await client.send(to: session, text: text, attachments: try await attachments.storedIDs(using: client))
            }
            sending = false
            if ok {
                message = ""
                attachments = Attachments()
                focused = false
                sentCount += 1
                fleet.notice = Notice(String(localized: "Message sent to \(node.displayName)"))
            }
        }
    }
}

/// One thread of a node: the main one or a subagent, with the task it works on.
private struct ThreadLine: View {
    let activity: NodeActivity
    let task: WorkTask?

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: activity.isMainThread ? "terminal" : "person.2")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .frame(width: 20)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 3) {
                HStack(alignment: .firstTextBaseline, spacing: 4) {
                    Text(activity.thread)
                        .font(.subheadline.weight(.semibold))
                    if !activity.isMainThread, let task {
                        Text("· subagent of “\(task.title)”")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                    Spacer(minLength: 4)
                    Text(relative(activity.since))
                        .font(.caption)
                        .foregroundStyle(.tertiary)
                }
                if let doing = activity.doing {
                    Text(doing)
                        .font(.mono(.footnote))
                        .foregroundStyle(Palette.sky)
                        .lineLimit(2)
                }
            }
        }
    }
}

extension FleetStore {
    @discardableResult
    func close(_ request: CloseRequest) async -> Bool {
        let ok = await perform { try await $0.closeNode(request.session, force: true) }
        if ok { notice = Notice(String(localized: "\(request.name) closed")) }
        return ok
    }

    /// Regular nodes, grouped by project in a stable, creation-time order; the orchestrator is
    /// never one of these rows (see `fleet.orchestrator`), so it never lands in a project group.
    var projectsByCreation: [(name: String, nodes: [Node])] {
        Dictionary(grouping: nodes.filter { !$0.isOrchestrator }) { $0.projectName ?? String(localized: "No project") }
            .map { (name: $0.key, nodes: $0.value.sorted(by: Self.creationOrder)) }
            .sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }

    /// Oldest node first; state does not drive the order, so a state change never moves a node.
    private static func creationOrder(_ a: Node, _ b: Node) -> Bool {
        if a.createdAt != b.createdAt { return a.createdAt < b.createdAt }
        return a.sessionId < b.sessionId
    }
}
