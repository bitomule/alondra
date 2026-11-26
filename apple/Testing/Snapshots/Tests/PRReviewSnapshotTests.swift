import AlondraContract
import AlondraDesign
import AlondraFeatures
import AlondraiOSUI
import ComposableArchitecture
import SnapshotTesting
import SwiftUI
import UIKit
import XCTest

/// The PR screen as the daemon's `pull_request:` view fills it.
@MainActor
final class PRReviewSnapshotTests: XCTestCase {
    private static let merge = """
    {"id":"merge:github.com/example/repo#12@3~abcdef12","label":"Merge","style":"primary","presentation":"menu",
     "options":[{"id":"merge","label":"Merge commit","style":"secondary","input":{"kind":"none"}},
                {"id":"squash","label":"squash","style":"secondary","input":{"kind":"none"}}],
     "input":{"kind":"none"},"attachments":false,"enabled":true}
    """

    private static let conflicts = """
    {"id":"conflicts:github.com/example/repo#12@3","label":"Resolve conflicts","style":"primary","presentation":"button",
     "input":{"kind":"none"},"attachments":false,"enabled":true}
    """

    /// The labels the daemon sends a Spanish phone (`crates/views/src/card.rs`); the fixture
    /// is the English document, so the Spanish variants swap them in to show what ships.
    private static let spanishLabels = [
        "Ask about the PR": "Preguntar por la PR",
        "Request changes": "Pedir cambios",
        "Merge": "Hacer merge",
        "Resolve conflicts": "Resolver conflictos",
        "Your question": "Tu pregunta",
        "Describe the changes": "Describe los cambios",
        "Answer “Ship the compact schedule editor with this beta?” first":
            "Responde primero a «¿Publicar el editor compacto con esta beta?»",
        "Another task is resolving these conflicts": "Otra tarea está resolviendo estos conflictos",
    ]

    private static let heldConflicts = """
    {"id":"conflicts:github.com/example/repo#12@3","label":"Resolve conflicts","style":"primary","presentation":"button",
     "input":{"kind":"none"},"attachments":false,"enabled":false,
     "disabled_reason":"Another task is resolving these conflicts"}
    """

    private func screen(
        adding action: String,
        holding held: [String: String] = [:],
        spanish: Bool = false
    ) throws -> some View {
        PRReviewView(store: store(try state(adding: action, holding: held, spanish: spanish)))
    }

    private func state(
        adding action: String,
        holding held: [String: String] = [:],
        spanish: Bool = false
    ) throws -> PRReviewFeature.State {
        let document = try ScreenFixtures.document("pull-request-ios") { object in
            var sections = object["sections"] as? [[String: Any]] ?? []
            for index in sections.indices where sections[index]["id"] as? String == "actions" {
                var bar = sections[index]["items"] as? [[String: Any]] ?? []
                var actions = bar[0]["actions"] as? [[String: Any]] ?? []
                if !action.isEmpty {
                    actions.append((try? JSONSerialization.jsonObject(with: Data(action.utf8)) as? [String: Any]) ?? [:])
                }
                for slot in actions.indices {
                    guard let verb = (actions[slot]["id"] as? String)?.split(separator: ":").first,
                          let reason = held[String(verb)] else { continue }
                    actions[slot]["enabled"] = false
                    actions[slot]["disabled_reason"] = reason
                }
                if spanish {
                    for slot in actions.indices {
                        if let label = actions[slot]["label"] as? String, let es = Self.spanishLabels[label] {
                            actions[slot]["label"] = es
                        }
                        if let reason = actions[slot]["disabled_reason"] as? String, let es = Self.spanishLabels[reason] {
                            actions[slot]["disabled_reason"] = es
                        }
                        if var input = actions[slot]["input"] as? [String: Any],
                           let placeholder = input["placeholder"] as? String, let es = Self.spanishLabels[placeholder] {
                            input["placeholder"] = es
                            actions[slot]["input"] = input
                        }
                    }
                }
                bar[0]["actions"] = actions
                sections[index]["items"] = bar
            }
            object["sections"] = sections
        }
        var state = PRReviewFeature.State(key: .pullRequest("github.com/example/repo#12"))
        state.screen.document = spanish ? try SpanishTestData.document(document) : document
        state.screen.phase = .loaded
        return state
    }

    private func composer(_ state: PRReviewFeature.State) -> some View {
        let actions = store(state).scope(state: \.actions, action: \.actions)
        return ActionComposerSheet(store: actions, title: actions.action?.label ?? "") {
            ActionInputView(store: actions, showCommentEditor: false)
        }
        .background(Palette.night)
    }

    private func store(_ state: PRReviewFeature.State) -> StoreOf<PRReviewFeature> {
        Store(initialState: state) { PRReviewFeature() } withDependencies: {
            idleViewsClient(&$0.viewsClient)
        }
    }

    func test_prReview_readyToMerge() throws {
        let view = try screen(adding: Self.merge)
        assertScreenVariants { view }
    }

    func test_prReview_conflicts() throws {
        let view = try screen(adding: Self.conflicts)
        assertScreenVariants { view }
    }

    func test_prReview_requestChangesSheet() throws {
        var state = try state(adding: Self.merge)
        state.actions.action = try XCTUnwrap(state.review?.requestChanges)
        state.actions.text = "Keep the weekday picker and add a test for it."
        let view = composer(state)
        assertScreenVariants(inNavigationStack: false) { view }
    }

    func test_prReview_spanishDaemonLabels() throws {
        let ready = try screen(adding: Self.merge, spanish: true)
        assertAppSnapshot(of: spanishScreen(ready), as: spanishImage(.dark), named: "merge_dark_es")
        let conflicts = try screen(adding: Self.conflicts, spanish: true)
        assertAppSnapshot(of: spanishScreen(conflicts), as: spanishImage(.light), named: "conflicts_light_es")
        var sheet = try state(adding: Self.merge, spanish: true)
        sheet.actions.action = try XCTUnwrap(sheet.review?.requestChanges)
        assertAppSnapshot(of: AnyView(composer(sheet).environment(\.locale, Locale(identifier: "es_ES"))),
                          as: spanishImage(.light), named: "sheet_light_es")
    }

    private func spanishScreen(_ view: some View) -> AnyView {
        AnyView(NavigationStack { view }.environment(\.locale, Locale(identifier: "es_ES")))
    }

    private func spanishImage(_ style: UIUserInterfaceStyle) -> Snapshotting<AnyView, UIImage> {
        Snapshotting<AnyView, UIImage>.image(drawHierarchyInKeyWindow: true, perceptualPrecision: 0.98,
               layout: .fixed(width: 393, height: 852), traits: .init(userInterfaceStyle: style)).asRecorded
    }

    /// The server's own view with Merge held back until a question is answered: the bar says why
    /// instead of offering a button that does nothing.
    func test_prReview_mergeHeldBackSaysWhy() throws {
        let view = try screen(adding: "")
        for style in [UIUserInterfaceStyle.light, .dark] {
            assertAppSnapshot(of: AnyView(NavigationStack { view }), as: spanishImage(style), named: style == .light ? "light" : "dark")
        }
        let spanish = try screen(adding: "", spanish: true)
        assertAppSnapshot(of: spanishScreen(spanish), as: spanishImage(.light), named: "light_es")
    }

    /// Request changes and Resolve conflicts the server holds back: each says why on the bar's
    /// glass, and neither is a grey button.
    func test_prReview_changesAndConflictsHeldBackSayWhy() throws {
        let held = ["changes": "Answer “Ship the compact schedule editor with this beta?” first"]
        let view = try screen(adding: Self.heldConflicts, holding: held)
        for style in [UIUserInterfaceStyle.light, .dark] {
            assertAppSnapshot(of: AnyView(NavigationStack { view }), as: spanishImage(style), named: style == .light ? "light" : "dark")
        }
        let spanish = try screen(adding: Self.heldConflicts, holding: held, spanish: true)
        assertAppSnapshot(of: spanishScreen(spanish), as: spanishImage(.light), named: "light_es")
    }

    func test_prReview_noLongerAvailable() {
        var state = PRReviewFeature.State(key: .pullRequest("github.com/example/repo#12"))
        state.screen.phase = .missing
        let view = PRReviewView(store: store(state))
        assertScreenVariants { view }
    }
}
