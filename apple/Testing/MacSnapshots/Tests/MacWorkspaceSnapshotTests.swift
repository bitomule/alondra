import AlondraContract
import AlondraDesign
import AlondraFeatures
import AlondraMacWorkspace
import AppKit
import ComposableArchitecture
import SnapshotTesting
import SwiftUI
import XCTest

private let referenceDate = Date(timeIntervalSince1970: 1_790_762_400)

private enum Appearance: CaseIterable {
    case light, dark

    var name: String { self == .light ? "light" : "dark" }
    var appearance: NSAppearance { NSAppearance(named: self == .light ? .aqua : .darkAqua)! }
}

/// The Mac window's screens as the daemon serves them to the Mac, drawn by the same views the app
/// shows and captured in light and dark.
@MainActor
final class MacWorkspaceSnapshotTests: XCTestCase {
    override func setUp() async throws {
        _ = NSApplication.shared
        RelativeClock.now = { referenceDate }
    }

    private func document(_ name: String) throws -> ViewDocument {
        let environment = ProcessInfo.processInfo.environment
        let root = URL(fileURLWithPath: try XCTUnwrap(environment["TEST_SRCDIR"]))
            .appendingPathComponent(environment["TEST_WORKSPACE"] ?? "_main")
        let url = root.appendingPathComponent("fixtures/views/cards/\(name).json")
        return try ContractCodec.decode(ViewDocument.self, from: Data(contentsOf: url))
    }

    private func presentation(
        sidebar: String = "sidebar-mac", detail: String? = nil, edit: (inout MacPresentationFeature.State) -> Void = { _ in }
    ) throws -> StoreOf<MacPresentationFeature> {
        var state = MacPresentationFeature.State()
        state.sidebarDocument = try document(sidebar)
        if let detail {
            let document = try document(detail)
            state.detailDocument = document
            state.selection = .viewKey(document.key)
            state.selectedViewKey = document.key
        }
        edit(&state)
        return Store(initialState: state) { MacPresentationFeature() }
    }

    private func assertWindow<Content: View>(
        size: CGSize, file: StaticString = #filePath, testName: String = #function, line: UInt = #line,
        @ViewBuilder _ content: () -> Content
    ) {
        let view = content()
        for variant in Appearance.allCases {
            let hosting = NSHostingView(rootView: view.background(Color(nsColor: .windowBackgroundColor)))
            let window = NSWindow(contentRect: CGRect(origin: .zero, size: size), styleMask: [.borderless], backing: .buffered, defer: false)
            window.appearance = variant.appearance
            window.contentView = hosting
            hosting.frame = CGRect(origin: .zero, size: size)
            hosting.layoutSubtreeIfNeeded()
            assertAppSnapshot(
                of: hosting as NSView,
                as: .image(perceptualPrecision: 0.98, size: size),
                named: variant.name, file: file, testName: testName, line: line
            )
        }
    }

    func test_sidebar_listsWhatWaitsForYou() throws {
        let store = try presentation(edit: { $0.selection = .viewKey("card:a98d034b-249e-4487-a89b-57de38c26372") })
        assertWindow(size: CGSize(width: 320, height: 880)) { MacServiceSidebar(presentation: store) }
    }

    func test_sidebar_hidesTheCardThatJustLeft() throws {
        let store = try presentation(edit: { state in
            let request = ActionRequest(commandId: "c1", dataEpoch: "", client: .mac, lang: "en", actionId: "answer")
            state.optimistic.begin("a98d034b-249e-4487-a89b-57de38c26372", request)
        })
        assertWindow(size: CGSize(width: 320, height: 880)) { MacServiceSidebar(presentation: store) }
    }

    func test_card_questionWithItsAnswers() throws {
        let store = try presentation(detail: "card-question-mac")
        assertWindow(size: CGSize(width: 900, height: 620)) { MacServiceDetail(presentation: store) }
    }

    func test_card_pullRequestWithItsShortcuts() throws {
        let store = try presentation(detail: "card-pr-mac")
        assertWindow(size: CGSize(width: 900, height: 700)) { MacServiceDetail(presentation: store) }
    }

    func test_card_noticeThatBlocksNothing() throws {
        let store = try presentation(detail: "card-information-mac")
        assertWindow(size: CGSize(width: 900, height: 520)) { MacServiceDetail(presentation: store) }
    }

    func test_card_clarificationSheet() throws {
        let store = try presentation(detail: "card-question-mac", edit: { state in
            let card = state.detailDocument?.sections.flatMap(\.items).compactMap { item -> Card? in
                if case let .card(card) = item { card } else { nil }
            }.first
            state.actionFeature.action = card?.more.first { $0.asksForClarification }
            state.actionFeature.selectedOptionID = "missing_context"
            state.actionFeature.text = "Which build do you mean?"
            state.actionPresented = true
        })
        assertWindow(size: CGSize(width: 460, height: 460)) { MacViewActionSheet(presentation: store) }
    }

    func test_pullRequest_detail() throws {
        let store = try presentation(detail: "pull-request-mac")
        assertWindow(size: CGSize(width: 900, height: 760)) { MacServiceDetail(presentation: store) }
    }

    func test_node_detail() throws {
        let store = try presentation(detail: "node-mac")
        assertWindow(size: CGSize(width: 900, height: 700)) { MacServiceDetail(presentation: store) }
    }

    func test_tasks_board() throws {
        let store = try presentation(detail: "tasks-mac")
        assertWindow(size: CGSize(width: 900, height: 520)) { MacServiceDetail(presentation: store) }
    }

    func test_fleet_overview() throws {
        let store = try presentation(detail: "fleet-mac")
        assertWindow(size: CGSize(width: 900, height: 520)) { MacServiceDetail(presentation: store) }
    }
}
