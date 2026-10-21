import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI
import XCTest

/// Settings › MCP servers as the daemon serves it to the iPhone: two projects with nodes, Linear
/// to sign in to in one and connected in the other, and two projects without a node, one checked.
@MainActor
final class McpServersSnapshotTests: XCTestCase {
    private func settings(_ document: ViewDocument) -> some View {
        var state = ServerSettingsFeature.State(key: .mcpServers)
        state.document = document
        let store = Store(initialState: state) { ServerSettingsFeature() }
        return NavigationStack {
            Form { ServerSettingsSection(store: store) }
                .scrollContentBackground(.hidden)
                .background(DawnBackground())
                .navigationTitle("Settings")
                .navigationBarTitleDisplayMode(.inline)
        }
    }

    func test_mcpServers() throws {
        let document = try SampleViews.document("mcp-servers-ios")
        assertScreen([.light, .dark]) { ScreenFrame(height: 1900) { settings(document) } }
    }

    func test_mcpServers_spanish() throws {
        let document = try SampleViews.document("mcp-servers-ios-es")
        assertScreen([.light], language: .spanish) { ScreenFrame(height: 1900) { settings(document) } }
    }
}
