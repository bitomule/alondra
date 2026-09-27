import AlondraContracts
import ComposableArchitecture
import SwiftUI

/// Reuses the workspace notice position; the recovery controls live in one shared sheet.
public struct DeliveryJournalButton: View {
    let store: StoreOf<DeliveryJournalFeature>
    public init(store: StoreOf<DeliveryJournalFeature>) { self.store = store }
    public var body: some View {
        Group {
            if !store.pending.isEmpty || store.error != nil {
                Button { store.send(.present(true)) } label: {
                    Label("Review deliveries", systemImage: "exclamationmark.arrow.triangle.2.circlepath")
                }
                .font(.footnote.weight(.semibold))
                .buttonStyle(.bordered)
            }
        }
        .sheet(isPresented: Binding(get: { store.presented }, set: { store.send(.present($0)) })) {
            DeliveryJournalView(store: store)
        }
    }
}

private struct DeliveryJournalView: View {
    let store: StoreOf<DeliveryJournalFeature>
    var body: some View {
        NavigationStack {
            List {
                Section {
                    Text("These commands reached your Mac. Delivery to Claude may still be uncertain. Check the terminal before deciding; retrying can duplicate an effect.")
                        .font(.callout).foregroundStyle(.secondary)
                    if let snapshot = store.snapshot { Text("Mac: \(snapshot.macID)").font(.caption).textSelection(.enabled) }
                }
                if let error = store.error { Section { Text(error).foregroundStyle(.orange) } }
                if store.pending.isEmpty {
                    Section { Text("No deliveries awaiting a decision") }
                }
                ForEach(store.pending) { attempt in
                    Section {
                        Button { store.send(.select(attempt.id)) } label: {
                            VStack(alignment: .leading, spacing: 4) {
                                Text("Delivery \(attempt.id) · \(attempt.channel)")
                                Text(attempt.sessionID).font(.caption).foregroundStyle(.secondary)
                                Text("\(attempt.state) · version \(attempt.version)").font(.caption)
                            }
                        }.buttonStyle(.plain).disabled(store.resolving)
                        if store.selected?.id == attempt.id {
                            if attempt.canResolve {
                                Toggle("I inspected this Mac’s terminal", isOn: Binding(get: { store.terminalChecked }, set: { store.send(.terminalChecked($0)) }))
                                Toggle("I accept that retrying may duplicate an effect", isOn: Binding(get: { store.duplicateRisk }, set: { store.send(.duplicateRisk($0)) }))
                                if ["menu_decision", "node_discard"].contains(attempt.channel) {
                                    Text("Retry reopens the question for a new decision. It does not repeat the previous action.").font(.caption)
                                }
                                Text("Discard clears this pending intent. It does not undo anything already done.").font(.caption).foregroundStyle(.secondary)
                                HStack {
                                    Button("Retry") { store.send(.resolve(retry: true)) }
                                        .disabled(!store.terminalChecked || !store.duplicateRisk || store.resolving)
                                    Button("Discard", role: .destructive) { store.send(.resolve(retry: false)) }
                                        .disabled(!store.terminalChecked || store.resolving)
                                    if store.resolving { ProgressView().controlSize(.small) }
                                }
                            } else { Text("Still dispatching. Wait for a final state before deciding.").font(.caption) }
                        }
                    }
                }
            }
            .navigationTitle("Deliveries")
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button("Done") { store.send(.present(false)) } }
                ToolbarItem { Button("Refresh", systemImage: "arrow.clockwise") { store.send(.refresh) }.disabled(store.resolving) }
            }
        }
        #if os(macOS)
        .frame(minWidth: 520, minHeight: 420)
        #endif
    }
}
