import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// The Fleet tab: the pace of the fleet, then every node grouped by project. Search is
/// answered by the daemon; a node closes from its row.
public struct FleetView: View {
    @Bindable var store: StoreOf<FleetFeature>
    @State private var opened: Set<String> = []
    @Environment(\.connectionState) private var connection

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
            if indicators.line != .summary {
                ConnectionLine(indicators.line, alert: .secondary) { EmptyView() }
                    .font(.footnote.weight(.medium))
                    .listRowBackground(Rectangle().fill(.thinMaterial))
                    .accessibilityIdentifier("fleet.savedDataConnectionLost")
            }
            ForEach(overview.groups) { group in
                let shown = !group.collapsed || opened.contains(group.id)
                Section {
                    if shown {
                        ForEach(group.nodes) { node in row(node) }
                    }
                } header: {
                    ProjectHeader(title: group.title, count: group.count,
                                  expansion: group.collapsed ? shown : nil) {
                        withAnimation(.smooth) { opened.formSymmetricDifference([group.id]) }
                    }
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
                if let pause = node.pause {
                    Button { store.send(.perform(pause)) } label: { Label(pause.label, systemImage: "pause.circle") }
                        .tint(Palette.slate)
                }
                if let resume = node.resume {
                    Button { store.send(.perform(resume)) } label: { Label(resume.label, systemImage: "play.circle") }
                        .tint(Palette.sky)
                }
            }
            .contextMenu {
                if let interrupt = node.interrupt {
                    Button { store.send(.perform(interrupt)) } label: { Label(interrupt.label, systemImage: "stop.circle") }
                }
                if let pause = node.pause {
                    Button { store.send(.perform(pause)) } label: { Label(pause.label, systemImage: "pause.circle") }
                }
                if let resume = node.resume {
                    Button { store.send(.perform(resume)) } label: { Label(resume.label, systemImage: "play.circle") }
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

    private var indicators: ConnectionIndicators {
        ConnectionIndicators(connection, hasSavedData: store.overview != nil)
    }

    @ViewBuilder private var placeholder: some View {
        switch (indicators.cover, store.screen.phase) {
        case (.lost(let problem), _):
            ConnectionUnavailable(problem: problem) { store.send(.screen(.retry)) }
        case (.connecting, _):
            ProgressView().tint(Palette.dawn).frame(maxWidth: .infinity, maxHeight: .infinity)
        case (.none, .failed(let message)):
            ContentUnavailableView {
                Label(message, systemImage: "exclamationmark.triangle")
            } actions: {
                Button("Try Again") { store.send(.screen(.retry)) }
            }
        case (.none, .missing):
            ContentUnavailableView("This screen is no longer available", systemImage: "questionmark.circle")
        case (.none, .loading), (.none, .loaded):
            ProgressView().tint(Palette.dawn).frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }
}

private struct ProjectHeader: View {
    let title: String?
    let count: UInt32?
    var expansion: Bool?
    var toggle: () -> Void = {}

    var body: some View {
        if let expansion {
            Button(action: toggle) { content(chevron: expansion) }
                .buttonStyle(.plain)
                .accessibilityValue(expansion ? Text("Expanded") : Text("Collapsed"))
        } else {
            content(chevron: nil)
        }
    }

    private func content(chevron: Bool?) -> some View {
        HStack(spacing: 8) {
            if let title {
                Text(title).font(.display(.title3, weight: .bold)).foregroundStyle(Palette.ink).textCase(nil)
            }
            Spacer()
            if let count, count > 0 {
                Text(count, format: .number).font(.caption.monospacedDigit()).foregroundStyle(.secondary)
            }
            if let chevron {
                Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                    .rotationEffect(.degrees(chevron ? 90 : 0))
            }
        }
        .padding(.top, 6)
        .contentShape(.rect)
    }
}

