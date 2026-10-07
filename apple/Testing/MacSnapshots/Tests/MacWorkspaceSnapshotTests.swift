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
        sidebar: String = "sidebar-mac", detail: String? = nil, spanish: Bool = false,
        edit: (inout MacPresentationFeature.State) -> Void = { _ in }
    ) throws -> StoreOf<MacPresentationFeature> {
        var state = MacPresentationFeature.State()
        state.sidebarDocument = try document(sidebar)
        if let detail {
            let document = spanish ? try SpanishTestData.document(try document(detail)) : try document(detail)
            state.detailDocument = document
            state.selection = .viewKey(document.key)
            state.selectedViewKey = document.key
        }
        edit(&state)
        return Store(initialState: state) { MacPresentationFeature() }
    }

    private func assertWindow<Content: View>(
        size: CGSize, spanish: Bool = false, variants: [Appearance] = Appearance.allCases, file: StaticString = #filePath, testName: String = #function, line: UInt = #line,
        @ViewBuilder _ content: () -> Content
    ) {
        let view = AnyView(content().environment(\.locale, spanish ? Locale(identifier: "es_ES") : .current))
        for variant in variants {
            let hosting = NSHostingView(rootView: view.background(Color(nsColor: .windowBackgroundColor)))
            let window = NSWindow(contentRect: CGRect(origin: .zero, size: size), styleMask: [.borderless], backing: .buffered, defer: false)
            window.appearance = variant.appearance
            window.contentView = hosting
            hosting.frame = CGRect(origin: .zero, size: size)
            hosting.layoutSubtreeIfNeeded()
            assertAppSnapshot(
                of: hosting as NSView,
                as: .image(perceptualPrecision: 0.98, size: size),
                named: spanish ? "\(variant.name)_es" : variant.name, file: file, testName: testName, line: line
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

    func test_card_questionOfSeveralAskedTogether() throws {
        let store = try presentation(detail: "card-group-mac")
        assertWindow(size: CGSize(width: 900, height: 620)) { MacServiceDetail(presentation: store) }
    }

    func test_card_pullRequestWithItsShortcuts() throws {
        let store = try presentation(detail: "card-pr-mac")
        assertWindow(size: CGSize(width: 900, height: 700)) { MacServiceDetail(presentation: store) }
    }

    func test_card_pullRequestWithItsShortcuts_spanish() throws {
        let store = try presentation(detail: "card-pr-es-mac", spanish: true)
        assertWindow(size: CGSize(width: 900, height: 700), spanish: true) { MacServiceDetail(presentation: store) }
    }

    func test_card_pullRequestBackSaysWhy() throws {
        let store = try presentation(detail: "card-pr-mac") {
            $0.returnReasons["hf-weekly-2026-09-30"] = "PR #11 must merge first: this one builds on it."
        }
        assertWindow(size: CGSize(width: 900, height: 720)) { MacServiceDetail(presentation: store) }
    }

    func test_card_pullRequestBackSaysWhy_spanish() throws {
        let store = try presentation(detail: "card-pr-es-mac", spanish: true) {
            $0.returnReasons["hf-weekly-2026-09-30"] = "No se pudo contactar con tu Mac. No se envió nada; inténtalo de nuevo."
        }
        assertWindow(size: CGSize(width: 900, height: 720), spanish: true) { MacServiceDetail(presentation: store) }
    }

    // MARK: What an action asks first, and what it says after

    /// The alert the system draws for a question an action asks: the view goes into a real window,
    /// and the sheet the alert opens on it is what is captured. The sheet's own view is drawn off
    /// screen once it is attached and laid out, so where its slide-in animation is does not matter.
    private func assertAlert<Content: View>(
        spanish: Bool, file: StaticString = #filePath, testName: String = #function, line: UInt = #line,
        @ViewBuilder _ content: () -> Content
    ) async {
        let view = content().environment(\.locale, spanish ? Locale(identifier: "es_ES") : Locale(identifier: "en_US"))
        let window = NSWindow(contentRect: CGRect(x: 0, y: 0, width: 460, height: 460), styleMask: [.titled],
                              backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.appearance = Appearance.light.appearance
        window.contentView = NSHostingView(rootView: view.background(Color(nsColor: .windowBackgroundColor)))
        window.orderFront(nil)
        let deadline = ContinuousClock.now.advanced(by: .seconds(5))
        while window.attachedSheet?.contentView == nil, ContinuousClock.now < deadline {
            try? await Task.sleep(for: .milliseconds(5))
        }
        guard let sheet = window.attachedSheet, let frame = sheet.contentView?.superview ?? sheet.contentView else {
            window.close()
            return XCTFail("no alert was presented", file: file, line: line)
        }
        frame.layoutSubtreeIfNeeded()
        let rep = frame.bitmapImageRepForCachingDisplay(in: frame.bounds)!
        frame.cacheDisplay(in: frame.bounds, to: rep)
        let image = NSImage(size: frame.bounds.size)
        image.addRepresentation(rep)
        window.endSheet(sheet)
        window.close()
        assertAppSnapshot(of: image, as: .image(perceptualPrecision: 0.98), named: spanish ? "light_es" : "light",
                          file: file, testName: testName, line: line)
    }

    private static let changedReply = [
        false: #"{"protocol":1,"outcome":"changed","result":{"kind":"changed","title":"PR #12 has new commits","body":"You reviewed an older version. Send anyway, or open the PR to see what changed.","next":{"id":"changes:github.com/example/repo#12@7~fedcba98","label":"Send anyway","style":"primary","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":true}}}"#,
        true: #"{"protocol":1,"outcome":"changed","result":{"kind":"changed","title":"La PR #12 tiene commits nuevos","body":"Revisaste una versión anterior. Envíalo igualmente o abre la PR para ver qué cambió.","next":{"id":"changes:github.com/example/repo#12@7~fedcba98","label":"Enviar igualmente","style":"primary","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":true}}}"#,
    ]

    private static let mergingReply = [
        false: #"{"protocol":1,"outcome":"applied","stamp":"Merging…","result":{"kind":"started","title":"The PR is merging","body":"You'll see it finished in its task when GitHub confirms."}}"#,
        true: #"{"protocol":1,"outcome":"applied","stamp":"Fusionando…","result":{"kind":"started","title":"La PR se está fusionando","body":"La verás terminada en su tarea cuando GitHub lo confirme."}}"#,
    ]

    private func pullRequest(spanish: Bool, _ edit: @escaping (inout ActionFeature.State, Card) throws -> Void) throws
        -> StoreOf<MacPresentationFeature> {
        var failure: (any Error)?
        let store = try presentation(detail: spanish ? "card-pr-es-mac" : "card-pr-mac", spanish: spanish) { state in
            guard let card = state.detailDocument?.sections.flatMap(\.items).compactMap({ item -> Card? in
                if case let .card(card) = item { card } else { nil }
            }).first else { return }
            do { try edit(&state.actionFeature, card) } catch { failure = error }
            state.actionPresented = true
        }
        if let failure { throw failure }
        return store
    }

    func test_closingWithoutMergingAsksFirstInRed() async throws {
        for spanish in [false, true] {
            let store = try pullRequest(spanish: spanish) { actions, card in
                let close = try XCTUnwrap(card.more.first { $0.id?.hasPrefix("close_unmerged:") == true })
                actions.action = close
                actions.confirmation = close.confirm
            }
            await assertAlert(spanish: spanish) { MacViewActionSheet(presentation: store) }
        }
    }

    func test_aPullRequestThatChangedAsksBeforeSendingAnyway() async throws {
        for spanish in [false, true] {
            let reply = try ContractCodec.decode(ActionResponse.self, from: Data(try XCTUnwrap(Self.changedReply[spanish]).utf8))
            guard case .ask(let changed) = OutcomeStyle(reply) else { return XCTFail("a changed answer asks") }
            let store = try pullRequest(spanish: spanish) { actions, card in
                actions.action = card.actions.first { $0.id?.hasPrefix("changes:") == true }
                actions.changed = changed
            }
            await assertAlert(spanish: spanish) { MacViewActionSheet(presentation: store) }
        }
    }

    func test_theNoticeSaysWhatTheServerSaid() throws {
        for spanish in [false, true] {
            let reply = try ContractCodec.decode(ActionResponse.self, from: Data(try XCTUnwrap(Self.mergingReply[spanish]).utf8))
            guard case .success(let title) = OutcomeStyle(reply) else { return XCTFail("a started merge is a notice") }
            var state = MacAppFeature.State()
            state.notice = WorkspaceNotice(event: .actionCompleted(title))
            let store = Store(initialState: state) { EmptyReducer<MacAppFeature.State, MacAppFeature.Action>() }
            assertWindow(size: CGSize(width: 520, height: 140), spanish: spanish, variants: [.light]) {
                MacNoticeToast(store: store).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottom)
            }
        }
    }

    func test_card_noticeThatBlocksNothing() throws {
        let store = try presentation(detail: "card-information-mac")
        assertWindow(size: CGSize(width: 900, height: 520)) { MacServiceDetail(presentation: store) }
    }

    func test_card_noticeWithACommandInItsBody_es() throws {
        let store = try presentation(detail: "card-know-command-es-mac")
        assertWindow(size: CGSize(width: 900, height: 620), spanish: true) { MacServiceDetail(presentation: store) }
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

    func test_connectionLost_signOutIncomplete() {
        assertWindow(size: CGSize(width: 900, height: 600)) {
            ConnectionLostView(problem: .signOutIncomplete, retry: {}, settings: {}, signOut: {})
        }
    }

    func test_connectionLost_signOutIncomplete_spanish() {
        assertWindow(size: CGSize(width: 900, height: 600), spanish: true, variants: [.light]) {
            ConnectionLostView(problem: .signOutIncomplete, retry: {}, settings: {}, signOut: {})
        }
    }

    func test_connectionLost_accessNotConfirmed() {
        assertWindow(size: CGSize(width: 900, height: 600)) {
            ConnectionLostView(problem: .accessNotConfirmed, retry: {}, settings: {}, pairAgain: {})
        }
    }

    func test_connectionLost_accessNotConfirmed_spanish() {
        assertWindow(size: CGSize(width: 900, height: 600), spanish: true, variants: [.light]) {
            ConnectionLostView(problem: .accessNotConfirmed, retry: {}, settings: {}, pairAgain: {})
        }
    }

    func test_connectionLost_deviceRevoked() {
        assertWindow(size: CGSize(width: 900, height: 600)) {
            ConnectionLostView(problem: .deviceRevoked(macName: "Mac Studio"), retry: {}, settings: {}, pairAgain: {})
        }
    }

    func test_connectionLost_deviceRevoked_spanish() {
        assertWindow(size: CGSize(width: 900, height: 600), spanish: true, variants: [.light]) {
            ConnectionLostView(problem: .deviceRevoked(macName: "Mac Studio"), retry: {}, settings: {}, pairAgain: {})
        }
    }

    func test_connectionLost_macChangedIdentity() {
        assertWindow(size: CGSize(width: 900, height: 600), variants: [.light]) {
            ConnectionLostView(problem: .macChangedIdentity, retry: {}, settings: {}, pairAgain: {})
        }
    }

    func test_connectionLost_macChangedIdentity_spanish() {
        assertWindow(size: CGSize(width: 900, height: 600), spanish: true, variants: [.light]) {
            ConnectionLostView(problem: .macChangedIdentity, retry: {}, settings: {}, pairAgain: {})
        }
    }
}
