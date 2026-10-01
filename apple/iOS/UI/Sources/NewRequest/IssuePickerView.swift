import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// The open issues to start a request from. A pasted issue link shows up as a row to choose.
public struct IssuePickerView: View {
    @Bindable var store: StoreOf<IssuePickerFeature>

    public init(store: StoreOf<IssuePickerFeature>) { self.store = store }

    public var body: some View {
        List {
            if let error = store.error {
                Label(error, systemImage: "exclamationmark.triangle")
                    .font(.footnote)
                    .foregroundStyle(Palette.rose)
            }
            ForEach(store.rows, id: \.id) { row in
                Button { store.send(.pick(row.id)) } label: {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(verbatim: row.title)
                            .foregroundStyle(Palette.ink)
                            .lineLimit(3)
                        Text(verbatim: ([row.subtitle].compactMap { $0 } + row.badges.map(\.text)).joined(separator: " · "))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                .buttonStyle(.plain)
            }
        }
        .scrollContentBackground(.hidden)
        .background(DawnBackground(glow: Palette.ember))
        .overlay {
            if store.loading && store.rows.isEmpty {
                ProgressView()
            } else if store.rows.isEmpty, let empty = store.empty {
                ContentUnavailableView(empty.text, systemImage: "magnifyingglass")
            }
        }
        .searchable(text: Binding(get: { store.query }, set: { store.send(.queryChanged($0)) }),
                    prompt: Text("Filter, or paste an issue link"))
        .refreshable { await store.send(.refresh).finish() }
        .navigationTitle("Issues")
        .task { await store.send(.task).finish() }
    }
}
