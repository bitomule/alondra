import AlondraContract
import AlondraFeed
import ComposableArchitecture
import SwiftUI

/// Settings' "Connection measurements": per path and network, how many requests went, how long
/// the answered ones took, and how many failed; the person can share or clear them.
public struct ConnectionMeasurementsSection: View {
    let store: StoreOf<ConnectionMeasurementsFeature>

    public init(store: StoreOf<ConnectionMeasurementsFeature>) { self.store = store }

    public var body: some View {
        Section {
            if store.summary.isEmpty {
                Text("No requests measured yet.").foregroundStyle(.secondary)
            } else {
                ForEach(store.summary) { group in
                    LabeledContent {
                        if let timing = group.timing { Text(timing).monospacedDigit() } else { Text(verbatim: "—") }
                    } label: {
                        Text(group.title)
                        if let failed = group.failed { Text("\(Text(group.requests)) · \(Text(failed))") } else { Text(group.requests) }
                    }
                }
                if let file = store.file {
                    ShareLink(item: file) { Label("Share measurements", systemImage: "square.and.arrow.up") }
                }
                Button("Clear measurements", role: .destructive) { store.send(.clear) }
            }
        } header: {
            Text("Connection measurements")
        } footer: {
            Text("How long each request to your Macs took, by path and network. Only times and paths are kept, nothing about your work.")
        }
        .task { await store.send(.task).finish() }
    }
}
