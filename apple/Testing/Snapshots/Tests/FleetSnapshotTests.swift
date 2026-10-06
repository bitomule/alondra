import AlondraContract
import AlondraFeatures
import AlondraiOSUI
import ComposableArchitecture
import SwiftUI
import XCTest

/// The Fleet tab and a node's screen as the daemon's `fleet` and `node:` views fill them.
@MainActor
final class FleetSnapshotTests: XCTestCase {
    private func fleet(document: String = "fleet-ios", phase: ScreenFeature.State.Phase = .loaded) throws -> some View {
        var state = FleetFeature.State()
        state.screen.document = try ScreenFixtures.document(document)
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

    func test_fleet_idleNodesSitCollapsedAtTheBottom() throws {
        let view = try fleet()
        assertScreen(inWindow: true) { ScreenFrame(height: 1150, growsWithText: false) { view } }
        let spanish = try fleet(document: "fleet-ios-es")
        assertScreen(language: .spanish, inWindow: true) { ScreenFrame(height: 1150, growsWithText: false) { spanish } }
    }

    private func fleetWithEveryState() throws -> some View {
        func node(_ id: String, _ title: String, name: String? = nil, _ state: (String, String),
                  activity: (String, String)? = nil, pending: Int = 0, badges: [[String: Any]] = []) -> [String: Any] {
            var row: [String: Any] = [
                "type": "row", "id": id, "icon": "node", "title": title, "depth": 0, "struck": false,
                "link": ["kind": "view", "key": "node:\(id)"],
                "badges": [["text": state.0, "tone": state.1]] + badges,
                "trailing": ["kind": "count", "value": pending],
            ]
            if let name { row["subtitle"] = name }
            if let activity {
                row["children"] = [[
                    "id": "activity", "icon": "notice", "title": activity.0, "depth": 0, "struck": false,
                    "trailing": ["kind": "time", "at": activity.1, "format": "relative"],
                ]]
            }
            return row
        }
        let document = try ScreenFixtures.document("fleet-ios") { object in
            guard var sections = object["sections"] as? [[String: Any]] else { return }
            var alondra = sections[1]
            alondra["id"] = "project:alondra"
            alondra["title"] = "alondra"
            alondra["count"] = 3
            alondra["items"] = [
                node("smoke", "Smoke test mods 0.2.26", ("Done", "success"), activity: ("OK", "2026-09-30T09:52:00Z")),
                node("home-zero", "Home shows everything at 0", name: "alondra-ios", ("Working", "info"),
                     activity: ("xcodebuild test -scheme Alondra", "2026-09-30T09:57:00Z"),
                     badges: [["text": "2 subagents", "tone": "neutral"]]),
                node("one-task", "One node, one task", ("Waiting for your review", "accent"), activity: ("PR #612 is ready", "2026-09-30T09:31:00Z"), pending: 1),
            ]
            sections[1]["count"] = 3
            sections[1]["items"] = [
                node("question", "Welcome copy for the beta", ("Waiting for an answer", "warning"),
                     activity: ("Which tone should the welcome use?", "2026-09-30T09:58:00Z"), pending: 1),
                node("stalled", "Import lists from Notes", name: "notes-import", ("Stalled", "critical"),
                     activity: ("swift build", "2026-09-30T08:40:00Z"),
                     badges: [["text": "1 shell · possibly hung", "tone": "warning"]]),
                node("idle", "Claude Code mods in Alondra", ("Idle", "neutral")),
            ]
            sections.insert(alondra, at: 1)
            object["sections"] = sections
        }
        var state = FleetFeature.State()
        state.screen.document = document
        state.screen.phase = .loaded
        return FleetView(store: Store(initialState: state) { FleetFeature() } withDependencies: {
            idleViewsClient(&$0.viewsClient)
        })
    }

    func test_fleet_nodesInReviewWorkingAndDone() throws {
        let view = try fleetWithEveryState()
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
