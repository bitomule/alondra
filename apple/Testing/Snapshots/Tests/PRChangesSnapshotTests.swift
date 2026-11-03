import AlondraContract
import AlondraDesign
import AlondraFeatures
import AlondraiOSUI
import ComposableArchitecture
import SwiftUI
import XCTest

/// A pull request's changes: folded into a summary on its card, then opened on their own screen
/// file by file, with the list of files to jump between them.
@MainActor
final class PRChangesSnapshotTests: XCTestCase {
    override func setUp() async throws { ScreenStores.pinTime() }

    private func files() throws -> [DiffFile] { UnifiedDiff.parse(try DiffFixture.text()) }

    private func detail(spanish: Bool) throws -> StoreOf<CardDetailFeature> {
        let card = spanish ? try Fixtures.spanishCard(Fixtures.pullRequestID) : try Fixtures.unblocked(try Fixtures.card(Fixtures.pullRequestID))
        let files = try files()
        return ScreenStores.cardDetail(card) { state in
            state.diff = files
            state.diffDigest = "abcdef12"
            state.collapsedFiles = Set(files.filter(\.startsCollapsed).map(\.id))
        }
    }

    func test_cardDetail_changesFoldedIntoTheirSummary() throws {
        let store = try detail(spanish: false)
        assertScreen(inWindow: true) { ScreenFrame(height: 852) { CardDetailView(store: store) } }
    }

    func test_cardDetail_changesFoldedIntoTheirSummary_es() throws {
        let store = try detail(spanish: true)
        assertScreen(language: .spanish, inWindow: true) { ScreenFrame(height: 852) { CardDetailView(store: store) } }
    }

    func test_cardDetail_changesStillLoadingShowTheCountsTheMacSent() throws {
        let store = ScreenStores.cardDetail(try Fixtures.unblocked(try Fixtures.card(Fixtures.pullRequestID))) {
            $0.diffDigest = "abcdef12"
        }
        assertScreen([.light], inWindow: true) { ScreenFrame(height: 852) { CardDetailView(store: store) } }
    }

    func test_changes_fileByFile() throws {
        let files = try files()
        let view = PRChangesView(files: files, truncated: true, collapsed: Set(files.filter(\.startsCollapsed).map(\.id)),
                                 comments: nil, toggle: { _ in }, toggleAll: {}, comment: { _, _ in }, retry: {})
            .environment(\.highlightsDiffs, false)
        assertScreenVariants { view }
    }

    func test_changes_withALineComment() throws {
        let files = try files()
        let anchor = try XCTUnwrap(files.first?.lines.first { $0.kind == .added }?.anchor)
        let view = PRChangesView(files: files, collapsed: Set(files.dropFirst().map(\.id)),
                                 comments: [anchor: ActionComment(anchor: anchor, text: "Keep the long rows behind a toggle", quote: "")],
                                 toggle: { _ in }, toggleAll: {}, comment: { _, _ in }, retry: {})
            .environment(\.highlightsDiffs, false)
        assertScreenVariants { view }
    }

    /// Long lines run off the edge and scroll sideways instead of wrapping.
    func test_changes_withoutWrappingLines() throws {
        let files = try files()
        let view = DiffViewer(files: files, collapsed: Set(files.dropFirst().map(\.id)), wrapsLines: false,
                              highlights: false, toggle: { _ in })
            .background(DawnBackground(glow: Palette.sky))
        assertScreen([.light, .dark]) { ScreenFrame(height: 500, growsWithText: false) { view } }
    }

    /// GitHub's «Viewed» boxes: a viewed file comes folded, one changed since it was viewed says so.
    private func marks(_ files: [DiffFile]) -> [String: FileViewed] {
        var marks = Dictionary(uniqueKeysWithValues: files.map { ($0.id, FileViewed.unviewed) })
        marks["Sources/CompactRow.swift"] = .viewed
        marks["Sources/Launch/Launch.swift"] = .viewed
        marks["Sources/ScheduleEditor.swift"] = .changed
        return marks
    }

    func test_changes_withViewedBoxes() throws {
        let files = try files()
        let marks = marks(files)
        let collapsed = Set(files.filter { $0.startsCollapsed || marks[$0.id] == .viewed }.map(\.id))
        let view = PRChangesView(files: files, collapsed: collapsed, comments: nil, marks: marks,
                                 toggle: { _ in }, toggleAll: {}, comment: { _, _ in }, retry: {})
            .environment(\.highlightsDiffs, false)
        assertScreenVariants { view }
    }

    func test_changes_viewedBoxGitHubDidNotKeep() throws {
        let files = try files()
        let view = PRChangesView(files: files, collapsed: Set(files.dropFirst().map(\.id)), comments: nil, marks: marks(files),
                                 failedMark: "Sources/LongForm.swift", toggle: { _ in }, toggleAll: {}, comment: { _, _ in }, retry: {})
            .environment(\.highlightsDiffs, false)
        assertScreen([.light]) { ScreenFrame(height: 852) { NavigationStack { view } } }
        assertScreen([.light], language: .spanish) { ScreenFrame(height: 852) { NavigationStack { view } } }
    }

    func test_changes_listOfFilesWithViewedBoxes() throws {
        let files = try files()
        let marks = marks(files)
        let list = List {
            Section {
                ForEach(files) { DiffFileRow(file: $0, viewed: marks[$0.id]) }
            } header: {
                DiffSummary.viewedCount(files, marks: marks)
            }
        }
        assertScreenVariants { list.navigationTitle("Files") }
    }

    private func detailWithViewedBoxes(spanish: Bool) throws -> StoreOf<CardDetailFeature> {
        let card = spanish ? try Fixtures.spanishCard(Fixtures.pullRequestID) : try Fixtures.unblocked(try Fixtures.card(Fixtures.pullRequestID))
        let files = try files()
        let marks = marks(files)
        return ScreenStores.cardDetail(card) { state in
            state.diff = files
            state.diffDigest = "abcdef12"
            state.viewed = ViewedFilesFeature.State(saved: FileMarks(prID: "github.com/example/repo#12", head: "abcdef12", files: marks))
        }
    }

    func test_cardDetail_changesSummaryCountsViewedFiles() throws {
        let store = try detailWithViewedBoxes(spanish: false)
        assertScreen([.light, .dark], inWindow: true) { ScreenFrame(height: 852) { CardDetailView(store: store) } }
    }

    func test_cardDetail_changesSummaryCountsViewedFiles_es() throws {
        let store = try detailWithViewedBoxes(spanish: true)
        assertScreen([.light, .dark], language: .spanish, inWindow: true) { ScreenFrame(height: 852) { CardDetailView(store: store) } }
    }

    func test_changes_listOfFiles() throws {
        let files = try files()
        let list = List(files) { DiffFileRow(file: $0) }
        assertScreenVariants { list.navigationTitle("Files") }
    }
}
