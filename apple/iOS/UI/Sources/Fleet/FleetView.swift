import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// The Fleet tab: the machine, the orchestrator, then every node grouped by project. Search is
/// answered by the daemon; a node closes from its row.
public struct FleetView: View {
    @Bindable var store: StoreOf<FleetFeature>

    public init(store: StoreOf<FleetFeature>) { self.store = store }

    public var body: some View {
        content
            .scrollContentBackground(.hidden)
            .background(DawnBackground(glow: Palette.sky))
            .navigationTitle("Fleet")
            .searchable(text: Binding(get: { store.query }, set: { store.send(.queryChanged($0)) }),
                        prompt: Text("Node, project or what it’s doing"))
            .refreshable { await store.send(.refresh).finish() }
            .task { await store.send(.task).finish() }
            .actionDialogs(store.scope(state: \.actions, action: \.actions))
    }

    @ViewBuilder private var content: some View {
        if store.isSearching {
            searchResults
        } else if let overview = store.overview {
            list(overview)
        } else {
            placeholder
        }
    }

    private func list(_ overview: FleetOverview) -> some View {
        List {
            if case .failed = store.screen.phase {
                Label("saved data · connection lost", systemImage: "wifi.slash")
                    .font(.footnote.weight(.medium)).foregroundStyle(.secondary)
                    .listRowBackground(Rectangle().fill(.thinMaterial))
                    .accessibilityIdentifier("fleet.savedDataConnectionLost")
            }
            if let orchestrator = overview.orchestrator {
                Section {
                    NodeRow(node: orchestrator, open: { store.send(.follow($0)) })
                        .listRowBackground(Palette.ember.opacity(0.12))
                }
                .listSectionSpacing(.compact)
            }
            ForEach(overview.groups) { group in
                Section {
                    ForEach(group.nodes) { node in row(node) }
                } header: {
                    ProjectHeader(title: group.title, count: group.count)
                }
            }
        }
        .listRowSpacing(10)
        .animation(.smooth, value: overview)
        .overlay {
            if overview.isEmpty, let empty = overview.empty {
                ContentUnavailableView(empty.text, systemImage: "circle.hexagongrid")
            }
        }
    }

    private func row(_ node: FleetOverview.Node) -> some View {
        NodeRow(node: node, open: { store.send(.follow($0)) })
            .listRowBackground(Palette.card.opacity(0.85))
            .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                if let close = node.close {
                    Button { store.send(.perform(close)) } label: { Label(close.label, systemImage: "xmark.bin") }
                        .tint(Palette.rose)
                }
                if let interrupt = node.interrupt {
                    Button { store.send(.perform(interrupt)) } label: { Label(interrupt.label, systemImage: "stop.circle") }
                        .tint(Palette.dawn)
                }
            }
            .contextMenu {
                if let interrupt = node.interrupt {
                    Button { store.send(.perform(interrupt)) } label: { Label(interrupt.label, systemImage: "stop.circle") }
                }
                if let close = node.close {
                    Button(role: .destructive) { store.send(.perform(close)) } label: {
                        Label(close.label, systemImage: "xmark.bin")
                    }
                }
            }
    }

    @ViewBuilder private var searchResults: some View {
        if let results = store.results {
            List {
                ForEach(results.groups) { group in
                    Section {
                        ForEach(group.nodes) { node in row(node) }
                    } header: {
                        if let title = group.title { Text(title).textCase(nil) }
                    }
                }
            }
            .listRowSpacing(10)
            .animation(.smooth, value: results)
            .overlay {
                if results.groups.isEmpty { ContentUnavailableView.search(text: store.query) }
            }
        } else if case .failed(let message)? = store.search?.phase {
            ContentUnavailableView(message, systemImage: "exclamationmark.triangle")
                .accessibilityIdentifier("fleet.searchFailed")
        } else {
            ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    @ViewBuilder private var placeholder: some View {
        switch store.screen.phase {
        case .failed(let message):
            ContentUnavailableView {
                Label(message, systemImage: "exclamationmark.triangle")
            } actions: {
                Button("Try Again") { store.send(.screen(.retry)) }
            }
        case .missing:
            ContentUnavailableView("This screen is no longer available", systemImage: "questionmark.circle")
        case .loading, .loaded:
            ProgressView().tint(Palette.dawn).frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }
}

private struct ProjectHeader: View {
    let title: String?
    let count: UInt32?

    var body: some View {
        HStack(spacing: 8) {
            if let title {
                Text(title).font(.display(.title3, weight: .bold)).foregroundStyle(Palette.ink).textCase(nil)
            }
            Spacer()
            if let count, count > 0 {
                Text(count, format: .number).font(.caption.monospacedDigit()).foregroundStyle(.secondary)
            }
        }
        .padding(.top, 6)
    }
}
