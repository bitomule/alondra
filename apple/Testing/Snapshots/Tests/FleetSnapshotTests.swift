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

    func test_fleet_nodesInReviewWorkingAndDone() throws {
        func node(_ id: String, _ title: String, _ state: (String, String), task: (String, String, String)) -> [String: Any] {
            [
                "type": "row", "id": id, "icon": "node", "title": title, "depth": 0, "struck": false,
                "link": ["kind": "view", "key": "node:\(id)"],
                "badges": [["text": state.0, "tone": state.1]],
                "trailing": ["kind": "count", "value": 0],
                "children": [[
                    "id": "t-\(id)", "icon": task.1, "title": task.0, "subtitle": task.2, "depth": 0, "struck": false,
                    "link": ["kind": "view", "key": "task:t-\(id)"],
                    "badges": [["text": task.2, "tone": task.1 == "check" ? "success" : "info"]],
                ]],
            ]
        }
        let document = try ScreenFixtures.document("fleet-ios") { object in
            guard var sections = object["sections"] as? [[String: Any]] else { return }
            sections[1]["count"] = 4
            sections[1]["items"] = [
                node("one-task", "One node, one task", ("Waiting for your review", "accent"),
                     task: ("One node, one task", "pull_request", "In Review")),
                node("recent-queue", "Real queue in recent requests", ("Done", "success"),
                     task: ("Real queue in recent requests", "check", "Done")),
                node("home-zero", "Home shows everything at 0", ("Working", "info"),
                     task: ("Home shows everything at 0", "mission", "Running")),
                node("claude-mods", "Claude Code mods in Alondra", ("Unclear what it wants", "unclear"),
                     task: ("Analyze Claude Code mods for Alondra", "check", "Done")),
            ]
            object["sections"] = sections
        }
        var state = FleetFeature.State()
        state.screen.document = document
        state.screen.phase = .loaded
        let view = FleetView(store: Store(initialState: state) { FleetFeature() } withDependencies: {
            idleViewsClient(&$0.viewsClient)
        })
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
