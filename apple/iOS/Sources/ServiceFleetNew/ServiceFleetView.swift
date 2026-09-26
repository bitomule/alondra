import AlondraAttachmentsFeature
import AlondraFleetFeature
import AlondraWorkspaceFeature
import AlondraTasksFeature
import ComposableArchitecture
import AlondraShared
import SwiftUI

struct ServiceFleetView: View {
    @Bindable var store: StoreOf<ServiceFleetFeature>
    var focusInbox: (String) -> Void = { _ in }
    private var sections: [FleetSection] { store.fleet.filteredSections }
    var body: some View {
        NavigationStack(path: $store.scope(state: \.path, action: \.path)) {
            List {
                if store.fleet.isStale { Text("Connection interrupted. Actions are paused until the fleet refreshes.").foregroundStyle(Palette.ember) }
                ForEach(Array(store.fleet.closeOutcomeByID.keys.sorted()), id: \.self) { id in
                    ServiceNodeOutcome(outcome: store.fleet.closeOutcomeByID[id])
                }
                if store.fleet.query.trimmingCharacters(in: .whitespaces).isEmpty, let orchestrator = store.fleet.snapshot?.orchestrator {
                    Section {
                        NavigationLink(state: ServiceFleetFeature.Path.State.node(NodeDetailRouteFeature.State(sessionID: orchestrator.id))) {
                            ServiceNodeRow(entry: orchestrator)
                        }
                        .listRowBackground(Palette.ember.opacity(0.12))
                    }
                    .listSectionSpacing(.compact)
                }
                ForEach(sections, id: \.name) { section in
                    Section {
                        ForEach(section.nodes) { node in
                            NavigationLink(state: ServiceFleetFeature.Path.State.node(NodeDetailRouteFeature.State(sessionID: node.id))) {
                                ServiceNodeRow(entry: node)
                            }
                            .listRowBackground(Palette.card.opacity(0.85))
                            .swipeActions(edge: .trailing) {
                                Button {
                                    store.send(.fleet(.closeRequested(node.id)))
                                } label: {
                                    Label("Close Node", systemImage: "xmark.bin")
                                }
                                .tint(Palette.rose)
                            }
                            .contextMenu {
                                Button(role: .destructive) {
                                    store.send(.fleet(.closeRequested(node.id)))
                                } label: {
                                    Label("Close Node", systemImage: "xmark.bin")
                                }
                            }
                        }
                    } header: {
                        ProjectHeader(name: section.name, nodes: section.nodes.map(\.node))
                    }
                }
            }
            .scrollContentBackground(.hidden)
            .background(DawnBackground(glow: Palette.sky))
            .overlay {
                if sections.isEmpty, store.overview != nil {
                    ContentUnavailableView.search(text: store.fleet.query)
                }
            }
            .animation(.smooth, value: sections.map(\.name))
            .navigationTitle("Fleet")
            .searchable(text: Binding(get: { store.fleet.query }, set: { store.send(.fleet(.searchChanged($0))) }), prompt: "Node, project or what it’s doing")
            .refreshable { await store.send(.refresh).finish() }
            .closeConfirmation(Binding(get: {
                guard let id = store.fleet.closeConfirmationID, let node = store.fleet.snapshot?.node(id)?.node else { return nil }
                return CloseRequest(session: id, name: node.displayName)
            }, set: { if $0 == nil { store.send(.fleet(.closeCancelled)) } })) { _ in
                store.send(.fleet(.closeConfirmed))
            }
        } destination: { child in
            switch child.case {
            case .node(let route):
                if let node = store.scope(state: \.nodes, action: \.nodes).first(where: { $0.sessionID == route.sessionID }) {
                    ServiceNodeView(store: node, presentation: store.presentation, openTask: { store.send(.pushTask($0)) }, focusInbox: { store.send(.focusInbox($0)) })
                }
            case .task(let task):
                ServiceTaskDetailView(store: task, presentation: store.presentation, openTask: { store.send(.pushTask($0)) }, focusInbox: { store.send(.focusInbox($0)) })
            }
        }
        .task { await store.send(.appeared).finish() }
        .onDisappear { store.send(.disappeared) }
        .onChange(of: store.focusedInboxID) { _, id in
            if let id { focusInbox(id); store.send(.inboxFocusHandled) }
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
                        .accessibilityLabel(occupancy.spoken(count))
                }
            }
        }
        .padding(.top, 6)
    }
}

private struct ServiceNodeRow: View {
    let entry: FleetNode
    private var node: Node { entry.node }
    private var pending: Int { entry.pendingCount }

    var body: some View {
        let tasks = entry.tasks
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
                    NodeTasks(tasks: tasks, nodeDisplayName: node.displayName)
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
