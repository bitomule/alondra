import AlondraFleetFeature
import AlondraShared
import AlondraWorkspaceFeature
import ComposableArchitecture
import SwiftUI

struct ServiceFleetView: View {
    @Bindable var store: StoreOf<ServiceFleetFeature>
    var body: some View {
        NavigationStack {
            List {
                if store.fleet.snapshot == nil { ProgressView("Loading fleet…") }
                if store.fleet.isStale { Text("Connection interrupted. Actions are paused until the fleet refreshes.").foregroundStyle(.orange) }
                if let orchestrator = store.fleet.snapshot?.orchestrator {
                    Section("Orchestrator") { node(orchestrator) }
                }
                ForEach(store.fleet.filteredSections) { section in
                    Section(section.name) { ForEach(section.nodes) { node($0) } }
                }
            }
            .searchable(text: Binding(get: { store.fleet.query }, set: { store.send(.fleet(.searchChanged($0))) }), prompt: "Find a node or task")
            .navigationTitle("Fleet")
            .navigationDestination(item: $store.scope(state: \.detail, action: \.detail)) { child in
                ServiceNodeView(store: child)
            }
        }
        .task { await store.send(.appeared).finish() }
        .onDisappear { store.send(.disappeared) }
    }
    private func node(_ entry: FleetNode) -> some View {
        Button { store.send(.openNode(entry.id)) } label: {
            VStack(alignment: .leading, spacing: 5) {
                Text(entry.node.displayName).font(.headline)
                Text(entry.node.summary).foregroundStyle(.secondary)
                Text("\(entry.tasks.count) tasks · \(entry.pendingCount) waiting").font(.caption)
            }.frame(maxWidth: .infinity, alignment: .leading)
        }.buttonStyle(.plain)
    }
}

private struct ServiceNodeView: View {
    let store: StoreOf<NodeDetailFeature>
    var body: some View {
        Form {
            if let node = store.node {
                Section {
                    Text(node.displayName).font(.title2.bold())
                    Text(node.summary)
                    if let cwd = node.cwd { Text(cwd).font(.caption.monospaced()).textSelection(.enabled) }
                }
                Section("Tasks") {
                    ForEach(store.tasks) { task in LabeledContent(task.title, value: task.state.label) }
                }
                Section("Activity") {
                    if store.isLoadingThreads { ProgressView("Refreshing activity…") }
                    if let error = store.threadsError { Text(error).foregroundStyle(.red) }
                    ForEach(store.threads, id: \.self) { activity in
                        VStack(alignment: .leading) {
                            Text(activity.thread).font(.headline)
                            if let task = activity.task { Text(task) }
                            if let doing = activity.doing { Text(doing).font(.caption.monospaced()) }
                        }
                    }
                }
                Section("Message") {
                    TextField("Message this node", text: Binding(get: { store.message }, set: { store.send(.messageChanged($0)) }), axis: .vertical)
                        .lineLimit(4...12)
                        .disabled(store.isSending || store.sendOutcome?.uncertainID != nil)
                    Button(store.isSending ? "Sending…" : "Send") { store.send(.sendTapped) }.disabled(!store.canSend)
                    ServiceNodeOutcome(outcome: store.sendOutcome)
                }
                Section {
                    Button("Close node", role: .destructive) { store.send(.closeRequested) }
                        .disabled(store.isStale || store.isClosing || store.isSending || store.closeOutcome?.uncertainID != nil || store.sendOutcome?.uncertainID != nil)
                    ServiceNodeOutcome(outcome: store.closeOutcome)
                }
            } else { ContentUnavailableView("This node has stopped", systemImage: "desktopcomputer") }
        }
        .navigationTitle("Node")
        .confirmationDialog("Close this node?", isPresented: Binding(get: { store.showCloseConfirmation }, set: { if !$0 { store.send(.closeCancelled) } })) {
            Button("Close node", role: .destructive) { store.send(.closeConfirmed) }
        } message: { Text("The service checks whether it is safe to stop the node before closing it.") }
        .task { await store.send(.appeared).finish() }
        .onDisappear { store.send(.disappeared) }
    }
}
private struct ServiceNodeOutcome: View {
    let outcome: NodeMutationOutcome?
    var body: some View {
        switch outcome {
        case .confirmed: Text("Completed").foregroundStyle(.secondary)
        case .rejected(let message): Text(message).foregroundStyle(.red)
        case .unknown: Text("The result is unknown. Check the node before doing anything else; your draft has been kept.").foregroundStyle(.orange)
        case .obsolete: Text("The connection changed. Reopen the node.")
        case nil: EmptyView()
        }
    }
}
