import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// The open issues a request can start from, on the Mac. A pasted issue link shows up as a row.
struct MacIssuePickerSheet: View {
    @Bindable var store: StoreOf<IssuePickerFeature>
    let cancel: () -> Void

    var body: some View {
        NavigationStack {
            List {
                if let error = store.error {
                    Label(error, systemImage: "exclamationmark.triangle").foregroundStyle(Palette.rose)
                }
                ForEach(store.rows, id: \.id) { row in
                    Button { store.send(.pick(row.id)) } label: {
                        VStack(alignment: .leading, spacing: 3) {
                            Text(verbatim: row.title).lineLimit(2)
                            Text(verbatim: ([row.subtitle].compactMap { $0 } + row.badges.map(\.text)).joined(separator: " · "))
                                .font(.caption).foregroundStyle(.secondary)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .contentShape(.rect)
                    }
                    .buttonStyle(.plain)
                }
            }
            .overlay {
                if store.loading && store.rows.isEmpty {
                    ProgressView()
                } else if store.rows.isEmpty, let empty = store.empty {
                    ContentUnavailableView(empty.text, systemImage: "magnifyingglass")
                }
            }
            .searchable(text: Binding(get: { store.query }, set: { store.send(.queryChanged($0)) }),
                        prompt: Text("Filter, or paste an issue link"))
            .navigationTitle("Issues")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel", action: cancel) }
            }
            .task { await store.send(.task).finish() }
        }
    }
}
