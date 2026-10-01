import AlondraContract
import AlondraFeatures
import AlondraiOSUI
import ComposableArchitecture
import SwiftUI
import XCTest

/// The Fleet tab and a node's screen as the daemon's `fleet` and `node:` views fill them.
@MainActor
final class FleetSnapshotTests: XCTestCase {
    private func fleet(phase: ScreenFeature.State.Phase = .loaded) throws -> some View {
        var state = FleetFeature.State()
        state.screen.document = try ScreenFixtures.document("fleet-ios")
        state.screen.phase = phase
        return FleetView(store: Store(initialState: state) { FleetFeature() } withDependencies: {
            idleViewsClient(&$0.viewsClient)
        })
    }

    private func node(draft: String = "") throws -> some View {
        let document = try ScreenFixtures.document("node-ios")
        var state = NodeDetailFeature.State(key: .node("d887a34b-249e-4487-a89b-57de38c26372"))
        state.screen.document = document
        state.screen.phase = .loaded
        state.composer.action = NodeDetail(document: document).message
        state.draft = draft
        return NodeDetailView(store: Store(initialState: state) { NodeDetailFeature() } withDependencies: {
            idleViewsClient(&$0.viewsClient)
        })
    }

    func test_fleet_orchestratorAndProjects() throws {
        let view = try fleet()
        assertScreenVariants { view }
    }

    func test_fleet_showingSavedDataWhileOffline() throws {
        let view = try fleet(phase: .failed("Couldn’t load this from your Mac. Try again."))
        assertScreenVariants { view }
    }

    func test_node_withActivityAndTheComposer() throws {
        let view = try node(draft: "Use the shorter welcome copy")
        assertScreenVariants { view }
    }
}
