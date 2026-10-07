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
    private static let diff = """
    diff --git a/Sources/ScheduleEditor.swift b/Sources/ScheduleEditor.swift
    --- a/Sources/ScheduleEditor.swift
    +++ b/Sources/ScheduleEditor.swift
    @@ -10,6 +10,7 @@ struct ScheduleEditor: View {
         var body: some View {
    -        Form { longForm }
    +        Form { compactRow }
    +        Button("Launch options") { showOptions = true }
         }
     }
    diff --git a/Cargo.lock b/Cargo.lock
    --- a/Cargo.lock
    +++ b/Cargo.lock
    @@ -1,2 +1,2 @@
    -version = 3
    +version = 4
    """

    private static let merge = """
    {"id":"merge:github.com/example/repo#12@3~abcdef12","label":"Merge","style":"primary","presentation":"menu",
     "options":[{"id":"merge","label":"Merge commit","style":"secondary","input":{"kind":"none"}},
                {"id":"squash","label":"squash","style":"secondary","input":{"kind":"none"}}],
     "input":{"kind":"none"},"attachments":false,
     "confirm":{"title":"Merge PR #12?","message":"Add the compact schedule editor","button":"Merge"},"enabled":true}
    """

    private static let conflicts = """
    {"id":"conflicts:github.com/example/repo#12@3","label":"Resolve conflicts","style":"primary","presentation":"button",
     "input":{"kind":"none"},"attachments":false,"enabled":true}
    """

    /// The labels the daemon sends a Spanish phone (`crates/core/src/views/card.rs`); the fixture
    /// is the English document, so the Spanish variants swap them in to show what ships.
    private static let spanishLabels = [
        "Ask about the PR": "Preguntar por la PR",
        "Request changes": "Pedir cambios",
        "Merge": "Fusionar",
        "Resolve conflicts": "Resolver conflictos",
        "Your question": "Tu pregunta",
        "Describe the changes": "Describe los cambios",
        "Answer “Ship the compact schedule editor with this beta?” first":
            "Responde primero a «¿Publicar el editor compacto con esta beta?»",
    ]

    private func screen(
        adding action: String,
        comments: [String: ActionComment] = [:],
        collapsed: Set<String> = [],
        spanish: Bool = false
    ) throws -> some View {
        PRReviewView(store: store(try state(adding: action, comments: comments, collapsed: collapsed, spanish: spanish)))
            .environment(\.highlightsDiffs, false)
    }

    private func state(
        adding action: String,
        comments: [String: ActionComment] = [:],
        collapsed: Set<String> = [],
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
        state.diffDigest = "abcdef12"
        state.diff = .loaded(UnifiedDiff.parse(Self.diff))
        state.comments = comments
        state.collapsed = collapsed
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

    func test_prReview_conflictsWithCommentsAndAFoldedFile() throws {
        let anchor = UnifiedDiff.parse(Self.diff).first?.lines.compactMap(\.anchor).first ?? ""
        let view = try screen(
            adding: Self.conflicts,
            comments: [anchor: ActionComment(anchor: anchor, text: "Does this keep the weekday picker?")],
            collapsed: ["Cargo.lock"]
        )
        assertScreenVariants { view }
    }

    func test_prReview_requestChangesSheetWithAComment() throws {
        let anchor = UnifiedDiff.parse(Self.diff).first?.lines.compactMap(\.anchor).first ?? ""
        let comment = ActionComment(anchor: anchor, text: "Does this keep the weekday picker?")
        var state = try state(adding: Self.merge, comments: [anchor: comment])
        state.actions.action = try XCTUnwrap(state.review?.requestChanges)
        state.actions.comments = [comment]
        state.actions.text = "Keep the weekday picker and add a test for it."
        let view = composer(state)
        assertScreenVariants(inNavigationStack: false) { view }
    }

    func test_prReview_spanishDaemonLabels() throws {
        let anchor = UnifiedDiff.parse(Self.diff).first?.lines.compactMap(\.anchor).first ?? ""
        let comment = ActionComment(anchor: anchor, text: "¿Mantiene el selector de días?")
        let ready = try screen(adding: Self.merge, comments: [anchor: comment], spanish: true)
        assertAppSnapshot(of: spanishScreen(ready), as: spanishImage(.dark), named: "merge_dark_es")
        let conflicts = try screen(adding: Self.conflicts, spanish: true)
        assertAppSnapshot(of: spanishScreen(conflicts), as: spanishImage(.light), named: "conflicts_light_es")
        var sheet = try state(adding: Self.merge, comments: [anchor: comment], spanish: true)
        sheet.actions.action = try XCTUnwrap(sheet.review?.requestChanges)
        sheet.actions.comments = [comment]
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

    func test_prReview_noLongerAvailable() {
        var state = PRReviewFeature.State(key: .pullRequest("github.com/example/repo#12"))
        state.screen.phase = .missing
        let view = PRReviewView(store: store(state))
        assertScreenVariants { view }
    }
}
