import AlondraContract
import AlondraDesign
import ComposableArchitecture
import SwiftUI

/// Settings' «MCP servers» row: the daemon's one-line summary of the screen it opens. A daemon that
/// sends no summary leaves the title alone.
public struct McpServersRow: View {
    let store: StoreOf<ServerSettingsFeature>

    public init(store: StoreOf<ServerSettingsFeature>) { self.store = store }

    public var body: some View {
        Section {
            NavigationLink {
                McpServersScreen(store: store)
            } label: {
                VStack(alignment: .leading, spacing: 2) {
                    Text("MCP servers").foregroundStyle(Palette.ink)
                    if let summary = store.document?.summary {
                        Text(verbatim: summary).font(.footnote).foregroundStyle(.secondary)
                    }
                }
            }
            .accessibilityIdentifier("settings.mcpServers")
        }
    }
}

/// Settings › MCP servers: the daemon's list of servers, each with what it offers, on a screen of its own.
public struct McpServersScreen: View {
    let store: StoreOf<ServerSettingsFeature>

    public init(store: StoreOf<ServerSettingsFeature>) { self.store = store }

    public var body: some View {
        Form { ServerSettingsSection(store: store, showsTitle: false) }
            .formStyle(.grouped)
            .navigationTitle(Text("MCP servers"))
            #if os(iOS)
            .scrollContentBackground(.hidden)
            .background(DawnBackground())
            .navigationBarTitleDisplayMode(.inline)
            #endif
    }
}
