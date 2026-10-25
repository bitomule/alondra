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

    func test_changes_listOfFiles() throws {
        let files = try files()
        let list = List(files) { DiffFileRow(file: $0) }
        assertScreenVariants { list.navigationTitle("Files") }
    }
}
