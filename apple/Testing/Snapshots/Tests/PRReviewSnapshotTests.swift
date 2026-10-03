import AlondraContract
import AlondraDesign
import AlondraFeatures
import AlondraiOSUI
import ComposableArchitecture
import SwiftUI
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

    private func screen(
        adding action: String,
        comments: [String: ActionComment] = [:],
        collapsed: Set<String> = []
    ) throws -> some View {
        PRReviewView(store: store(try state(adding: action, comments: comments, collapsed: collapsed)))
            .environment(\.highlightsDiffs, false)
    }

    private func state(
        adding action: String,
        comments: [String: ActionComment] = [:],
        collapsed: Set<String> = []
    ) throws -> PRReviewFeature.State {
        let document = try ScreenFixtures.document("pull-request-ios") { object in
            var sections = object["sections"] as? [[String: Any]] ?? []
            for index in sections.indices where sections[index]["id"] as? String == "actions" {
                var bar = sections[index]["items"] as? [[String: Any]] ?? []
                var actions = bar[0]["actions"] as? [[String: Any]] ?? []
                actions.append((try? JSONSerialization.jsonObject(with: Data(action.utf8)) as? [String: Any]) ?? [:])
                bar[0]["actions"] = actions
                sections[index]["items"] = bar
            }
            object["sections"] = sections
        }
        var state = PRReviewFeature.State(key: .pullRequest("github.com/example/repo#12"))
        state.screen.document = document
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

    func test_prReview_askSheet() throws {
        var state = try state(adding: Self.merge)
        state.actions.action = try XCTUnwrap(state.review?.ask)
        let view = composer(state)
        assertScreenVariants(inNavigationStack: false) { view }
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

    func test_prReview_noLongerAvailable() {
        var state = PRReviewFeature.State(key: .pullRequest("github.com/example/repo#12"))
        state.screen.phase = .missing
        let view = PRReviewView(store: store(state))
        assertScreenVariants { view }
    }
}
