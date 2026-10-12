import ComposableArchitecture
import SwiftUI

/// A Mac's folders as its daemon lists them, to pick the projects folder without typing a path.
public struct FolderBrowserView: View {
    let store: StoreOf<FolderBrowserFeature>
    @Environment(\.dismiss) private var dismiss

    public init(store: StoreOf<FolderBrowserFeature>) { self.store = store }

    public var body: some View {
        List {
            if let listing = store.listing {
                Section {
                    if let parent = listing.parent {
                        Button { store.send(.open(parent)) } label: {
                            Label(FolderPath.display(parent), systemImage: "arrow.turn.left.up")
                        }
                        .accessibilityIdentifier("folderBrowser.up")
                    }
                    ForEach(listing.folders) { folder in
                        Button { store.send(.open(folder.path)) } label: {
                            HStack {
                                Label(folder.name, systemImage: folder.repo ? "shippingbox" : "folder")
                                Spacer()
                                if folder.repos > 0 { Text("\(folder.repos) repos").foregroundStyle(.secondary) }
                            }
                            .contentShape(.rect)
                        }
                        .buttonStyle(.plain)
                    }
                } header: {
                    Text(verbatim: FolderPath.display(listing.path)).font(.body.monospaced()).textCase(nil)
                }
            } else if store.loading {
                ProgressView().frame(maxWidth: .infinity)
            }
            if let error = store.error {
                Text(verbatim: error).foregroundStyle(.red)
            }
        }
        .navigationTitle("Projects folder")
        #if os(iOS)
        .navigationBarTitleDisplayMode(.inline)
        #endif
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button("Cancel") { dismiss() }
            }
            ToolbarItem(placement: .confirmationAction) {
                Button("Use This Folder") { store.send(.use) }
                    .disabled(store.listing == nil || store.loading)
                    .accessibilityIdentifier("folderBrowser.use")
            }
        }
        .task { await store.send(.task).finish() }
    }
}
