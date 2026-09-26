import AlondraAttachmentsFeature
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

struct ServiceNodeOutcome: View {
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
