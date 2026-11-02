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

    private func document(_ name: String, folder: String = "cards") throws -> ViewDocument {
        let environment = ProcessInfo.processInfo.environment
        let root = URL(fileURLWithPath: try XCTUnwrap(environment["TEST_SRCDIR"]))
            .appendingPathComponent(environment["TEST_WORKSPACE"] ?? "_main")
        let url = root.appendingPathComponent("fixtures/views/\(folder)/\(name).json")
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
        return Store(initialState: state) { MacPresentationFeature() } withDependencies: {
            $0.viewsClient.fetchDiff = { _ in try await Task.never() }
        }
    }

    private func assertWindow<Content: View>(
        size: CGSize, spanish: Bool = false, alignTop: Bool = false, variants: [Appearance] = Appearance.allCases, file: StaticString = #filePath, testName: String = #function, line: UInt = #line,
        @ViewBuilder _ content: () -> Content
    ) {
        let view = AnyView(content().environment(\.locale, spanish ? Locale(identifier: "es_ES") : .current))
        for variant in variants {
            let hosting = NSHostingView(rootView: view.background(Color(nsColor: .windowBackgroundColor)))
            let window = snapshotWindow(size: size)
            defer { window.close() }
            window.appearance = variant.appearance
            window.contentView = hosting
            hosting.frame = CGRect(origin: .zero, size: size)
            hosting.layoutSubtreeIfNeeded()
            if alignTop {
                window.makeFirstResponder(nil)
                alignSnapshotViewportToTop(hosting)
            }
            assertAppSnapshot(
                of: hosting as NSView,
                as: .image(perceptualPrecision: 0.98, size: size),
                named: spanish ? "\(variant.name)_es" : variant.name, file: file, testName: testName, line: line
            )
        }
    }

    private static let margin = #"{"id":"claude_margin","label":"Claude · margin","value":0.35,"text":"35%","detail":"Room left in the week, today's share and the 5-hour window; work only waits when every account is at its limit · the week resets Monday at 02:00","level":"calm","parts":[{"label":"Principal","value":0.7,"text":"70%","detail":"week 30% · 5 h 10% · today 2/15%","level":"calm"},{"label":"Trabajo","value":0.0,"text":"0%","detail":"week 60% · 5 h 95% · today 4/8%","level":"warm"},{"label":"API","value":0.0,"text":"per use","detail":"Paid per use: only requests that choose it run on it","level":"calm"}]}"#
    private static let margen = #"{"id":"claude_margin","label":"Claude · margen","value":0.0,"text":"0%","detail":"Todas las cuentas de Claude están en su límite: el trabajo nuevo espera","level":"hot","parts":[{"label":"Principal","value":0.0,"text":"límite","detail":"En su límite; vuelve en 2 h (14:30) · semana 100% · 5 h 40%","level":"hot"},{"label":"Trabajo","value":0.0,"text":"sin sesión","detail":"Inicia sesión en el Mac: alondra accounts login \"Trabajo\"","level":"hot"}]}"#

    func test_claudeMargin_opensOneRowPerAccount() throws {
        let gauge = try ContractCodec.decode(WireGauge.self, from: Data(Self.margin.utf8))
        assertWindow(size: CGSize(width: 340, height: 300)) { MacGaugeParts(gauge: gauge, parts: gauge.parts ?? []) }
    }

    func test_claudeMargin_whenEveryAccountIsAtItsLimit_es() throws {
        let gauge = try ContractCodec.decode(WireGauge.self, from: Data(Self.margen.utf8))
        assertWindow(size: CGSize(width: 340, height: 240), spanish: true) { MacGaugeParts(gauge: gauge, parts: gauge.parts ?? []) }
    }

    func test_sidebar_listsWhatWaitsForYou() throws {
        let store = try presentation(edit: { $0.selection = .viewKey("card:a98d034b-249e-4487-a89b-57de38c26372") })
        assertWindow(size: CGSize(width: 320, height: 880)) { MacServiceSidebar(presentation: store) }
    }

    /// A card arrived while another was open: the open one stays selected and the header says so.
    func test_sidebar_announcesACardThatArrivedWhileReading() throws {
        for spanish in [false, true] {
            let store = try presentation(spanish: spanish, edit: { state in
                let waiting = MacPresentationFeature.waitingKeys(in: state.sidebarDocument)
                state.selection = waiting.dropFirst().first.map(MacPresentationFeature.Selection.viewKey)
                state.selectedViewKey = waiting.dropFirst().first
                state.arrivals = Array(waiting.prefix(1))
            })
            assertWindow(size: CGSize(width: 320, height: 880), spanish: spanish) {
                MacServiceSidebar(presentation: store)
            }
        }
    }

    func test_sidebar_hidesTheCardThatJustLeft() throws {
        let store = try presentation(edit: { state in
            let request = ActionRequest(commandId: "c1", dataEpoch: "", client: .mac, lang: "en", actionId: "answer")
            state.optimistic.begin("a98d034b-249e-4487-a89b-57de38c26372", request)
        })
        assertWindow(size: CGSize(width: 320, height: 880)) { MacServiceSidebar(presentation: store) }
    }

    /// An answer the daemon never confirmed: its card is listed again, plain, and the notice says
    /// which answer did not arrive.
    func test_sidebar_anUnconfirmedAnswerBringsItsCardBackSayingWhich() throws {
        for spanish in [false, true] {
            let id = "a98d034b-249e-4487-a89b-57de38c26372"
            let presentation = try presentation(edit: { state in
                let request = ActionRequest(commandId: "c1", dataEpoch: "", client: .mac, lang: "en", actionId: "answer:\(id)@1", resolvesCard: true)
                state.optimistic.begin(id, request)
                state.optimistic.expire(id)
            })
            XCTAssertFalse(presentation.state.hides(id))
            let title = try XCTUnwrap(presentation.state.sidebarDocument?.sections.flatMap(\.items).lazy.compactMap { item -> Row? in
                if case let .row(row) = item, row.id == id { row } else { nil }
            }.first?.title)
            var state = MacAppFeature.State()
            state.macPresentation = presentation.state
            state.notice = WorkspaceNotice(event: .failure(spanish ? "Tu respuesta a «\(title)» no ha llegado a tu Mac."
                : Optimistic.notConfirmed(title)))
            let store = Store(initialState: state) { EmptyReducer<MacAppFeature.State, MacAppFeature.Action>() }
            assertWindow(size: CGSize(width: 420, height: 880), spanish: spanish, variants: [.light]) {
                MacServiceSidebar(presentation: store.scope(state: \.macPresentation, action: \.macPresentation))
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .overlay(alignment: .bottom) { MacNoticeToast(store: store) }
            }
        }
    }

    func test_settings_mcpServers() throws {
        for spanish in [false, true] {
            var state = ServerSettingsFeature.State(key: .mcpServers, client: .mac)
            state.document = try document(spanish ? "mcp-servers-ios-es" : "mcp-servers-mac")
            let store = Store(initialState: state) { ServerSettingsFeature() }
            assertWindow(size: CGSize(width: 640, height: 1200), spanish: spanish, variants: spanish ? [.light] : Appearance.allCases) {
                McpServersScreen(store: store)
            }
        }
    }

    func test_settings_claudeAccounts() throws {
        for spanish in [false, true] {
            var state = ServerSettingsFeature.State(key: .accounts, client: .mac)
            state.document = try document(spanish ? "accounts-mac-es" : "accounts-mac")
            let store = Store(initialState: state) { ServerSettingsFeature() }
            assertWindow(size: CGSize(width: 640, height: 760), spanish: spanish, variants: spanish ? [.light] : Appearance.allCases) {
                Form { ServerSettingsSection(store: store) }.formStyle(.grouped)
            }
        }
    }

    func test_settings_claudeSignInInTerminal() {
        assertWindow(size: CGSize(width: 460, height: 240)) {
            ClaudeSignInSheet(signIn: .init(command: "alondra accounts login 'Personal'", terminalOpen: true)) {}
        }
    }

    private func mcpSignIn(_ phase: McpSignInFeature.Phase, edit: (inout McpSignInFeature.State) -> Void = { _ in }) -> some View {
        var state = McpSignInFeature.State(card: "mcp-sign-in-t-7", server: "Linear")
        state.phase = phase
        edit(&state)
        let store = Store(initialState: state) { McpSignInFeature() } withDependencies: {
            $0.viewsClient.observe = { _ in AsyncThrowingStream { _ in } }
        }
        return McpSignInSheet(store: store)
    }

    func test_mcpSignIn_listening() {
        assertWindow(size: CGSize(width: 460, height: 280)) { mcpSignIn(.signingIn(.listening)) }
    }

    /// The paste state is captured on the iPhone only: on the Mac, the paste button follows the real pasteboard.
    func test_mcpSignIn_claudeAi_spanish() {
        assertWindow(size: CGSize(width: 460, height: 280), spanish: true, variants: Appearance.allCases) { mcpSignIn(.claudeAi) }
    }

    func test_mcpSignIn_sending_spanish() {
        assertWindow(size: CGSize(width: 460, height: 280), spanish: true, variants: [.light]) { mcpSignIn(.sending) }
    }

    func test_mcpSignIn_cut_spanish() {
        assertWindow(size: CGSize(width: 460, height: 280), spanish: true, variants: Appearance.allCases) { mcpSignIn(.failed(.cut)) }
    }

    func test_card_questionWithItsAnswers() throws {
        let store = try presentation(detail: "card-question-mac")
        assertWindow(size: CGSize(width: 900, height: 620)) { MacServiceDetail(presentation: store) }
    }

    private func newerDaemonCard(spanish: Bool = false) throws -> StoreOf<MacPresentationFeature> {
        try presentation(sidebar: spanish ? "sidebar-mac-es" : "sidebar-mac") { state in
            let card = try? self.document(spanish ? "card-es" : "card", folder: "newer-daemon")
            state.detailDocument = card
            state.selection = card.map { .viewKey($0.key) }
            state.selectedViewKey = card?.key
        }
    }

    func test_card_fromANewerDaemonAsksToUpdateForWhatItCannotUse() throws {
        let store = try newerDaemonCard()
        assertWindow(size: CGSize(width: 900, height: 760)) { MacServiceDetail(presentation: store) }
    }

    func test_card_fromANewerDaemonAsksToUpdateForWhatItCannotUse_spanish() throws {
        let store = try newerDaemonCard(spanish: true)
        assertWindow(size: CGSize(width: 900, height: 760), spanish: true, variants: [.light]) { MacServiceDetail(presentation: store) }
    }

    func test_card_questionOfSeveralAskedTogether() throws {
        let store = try presentation(detail: "card-group-mac")
        assertWindow(size: CGSize(width: 900, height: 620)) { MacServiceDetail(presentation: store) }
    }

    func test_card_pullRequestWithItsShortcuts() throws {
        let store = try presentation(detail: "card-pr-mac")
        assertWindow(size: CGSize(width: 900, height: 700)) { MacServiceDetail(presentation: store) }
    }

    /// The daemon's own Spanish sidebar, its first card drawn from the cards it carries: nothing
    /// else was asked of the Mac.
    func test_card_theFirstCardComesWithTheSidebar_spanish() throws {
        let store = try presentation(sidebar: "sidebar-mac-es") { state in
            let first = state.sidebarDocument?.firstCards.first
            XCTAssertEqual(first?.key, "card:a98d034b-249e-4487-a89b-57de38c26372")
            state.detailDocument = try? first.map(SpanishTestData.document)
            state.selection = first.map { .viewKey($0.key) }
            state.selectedViewKey = first?.key
        }
        assertWindow(size: CGSize(width: 900, height: 700), spanish: true, variants: [.light]) { MacServiceDetail(presentation: store) }
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

    func test_card_aReplyWaitingForAPausedNodeSaysSo() throws {
        let store = try presentation(detail: "card-reply-waiting-mac")
        assertWindow(size: CGSize(width: 900, height: 720)) { MacServiceDetail(presentation: store) }
    }

    func test_card_aReplyWaitingForAPausedNodeSaysSo_spanish() throws {
        let store = try presentation(detail: "card-reply-waiting-es-mac", spanish: true)
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
        let window = snapshotWindow(size: CGSize(width: 460, height: 460), styleMask: [.titled])
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
        true: #"{"protocol":1,"outcome":"applied","stamp":"Haciendo merge…","result":{"kind":"started","title":"Se está haciendo merge de la PR","body":"La verás terminada en su tarea cuando GitHub lo confirme."}}"#,
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
                actions.action = card.reply
                actions.changed = changed
            }
            await assertAlert(spanish: spanish) { MacViewActionSheet(presentation: store) }
        }
    }

    func test_answersSetAsideSayWhatToAnswerAgain() {
        for spanish in [false, true] {
            assertWindow(size: CGSize(width: 680, height: 170), spanish: spanish, variants: spanish ? [.light] : Appearance.allCases) {
                SetAsideAnswersBanner(cards: .waiting(2), othersMayBeMissing: true) {}
                    .frame(maxWidth: 640).padding(16)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            }
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

    /// A notification for a card answered elsewhere: the card is never drawn, the detail closes and
    /// the toast says what happened, in the words the daemon sent.
    func test_aSettledCardClosesSayingWhy() async throws {
        for spanish in [false, true] {
            let settled = try document(spanish ? "card-settled-mac-es" : "card-settled-mac")
            let why = try XCTUnwrap(settled.settledNotice)
            let presentation = TestStore(initialState: MacPresentationFeature.State()) { MacPresentationFeature() } withDependencies: {
                $0.viewsClient.load = { _, _ in .document(settled) }
                $0.viewsClient.openScreen = { _, _ in }
            }
            presentation.exhaustivity = .off
            await presentation.send(.select(.viewKey(settled.key)))
            await presentation.receive(\.settled, why)
            XCTAssertNil(presentation.state.detailDocument)
            var state = MacAppFeature.State()
            state.macPresentation = presentation.state
            state.notice = WorkspaceNotice(event: .actionCompleted(why))
            let store = Store(initialState: state) { EmptyReducer<MacAppFeature.State, MacAppFeature.Action>() }
            assertWindow(size: CGSize(width: 900, height: 520), spanish: spanish, variants: [.light]) {
                MacServiceDetail(presentation: store.scope(state: \.macPresentation, action: \.macPresentation))
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .overlay(alignment: .bottom) { MacNoticeToast(store: store) }
            }
        }
    }

    func test_card_deliveryWithItsApprovedPlan() throws {
        let store = try presentation(detail: "card-delivery-checklist-mac")
        assertWindow(size: CGSize(width: 900, height: 820)) { MacServiceDetail(presentation: store) }
    }

    func test_card_deliveryWithItsApprovedPlan_spanish() throws {
        let store = try presentation(detail: "card-delivery-checklist-es-mac", spanish: true)
        assertWindow(size: CGSize(width: 900, height: 820), spanish: true, alignTop: true) { MacServiceDetail(presentation: store) }
    }

    func test_card_deliveryThatProposesAPlan_spanish() throws {
        let store = try presentation(detail: "card-delivery-plan-es-mac", spanish: true)
        assertWindow(size: CGSize(width: 900, height: 900), spanish: true, alignTop: true) { MacServiceDetail(presentation: store) }
    }

    func test_card_secret() throws {
        let store = try presentation(detail: "card-secret-mac", spanish: false)
        assertWindow(size: CGSize(width: 900, height: 700), spanish: false, alignTop: true) { MacServiceDetail(presentation: store) }
    }

    func test_card_secret_spanish() throws {
        let store = try presentation(detail: "card-secret-es-mac", spanish: true)
        assertWindow(size: CGSize(width: 900, height: 700), spanish: true, alignTop: true) { MacServiceDetail(presentation: store) }
    }

    func test_card_reportAlone_spanish() throws {
        let store = try presentation(detail: "card-delivery-report-es-mac", spanish: true)
        assertWindow(size: CGSize(width: 900, height: 820), spanish: true, alignTop: true) { MacServiceDetail(presentation: store) }
    }

    func test_card_withItsDocuments() throws {
        let store = try presentation(detail: "card-documents-mac", spanish: false)
        assertWindow(size: CGSize(width: 900, height: 900), spanish: false, alignTop: true) { MacServiceDetail(presentation: store) }
    }

    func test_card_withItsDocuments_spanish() throws {
        let store = try presentation(detail: "card-documents-es-mac", spanish: true)
        assertWindow(size: CGSize(width: 900, height: 900), spanish: true, alignTop: true) { MacServiceDetail(presentation: store) }
    }

    /// The plan card the e2e sandbox's daemon served (`make e2e-plan-card CLIENT=mac`): the file the
    /// plan cites is shown at its end.
    func test_card_planWithTheFileItCites_es() throws {
        let store = try presentation(detail: "card-plan-cited-file-es-mac", spanish: true)
        assertWindow(size: CGSize(width: 900, height: 1500), spanish: true, alignTop: true) { MacServiceDetail(presentation: store) }
    }

    func test_card_noticeThatBlocksNothing() throws {
        let store = try presentation(detail: "card-information-mac")
        assertWindow(size: CGSize(width: 900, height: 520)) { MacServiceDetail(presentation: store) }
    }

    func test_card_outcomeAnswer() throws {
        let store = try presentation(detail: "card-outcome-answer-mac")
        assertWindow(size: CGSize(width: 900, height: 560)) { MacServiceDetail(presentation: store) }
    }

    func test_card_outcomeAnswer_es() throws {
        let store = try presentation(detail: "card-outcome-answer-es-mac", spanish: true)
        assertWindow(size: CGSize(width: 900, height: 560), spanish: true) { MacServiceDetail(presentation: store) }
    }

    func test_card_outcomeDone() throws {
        let store = try presentation(detail: "card-outcome-done-mac")
        assertWindow(size: CGSize(width: 900, height: 560)) { MacServiceDetail(presentation: store) }
    }

    func test_card_outcomeDone_es() throws {
        let store = try presentation(detail: "card-outcome-done-es-mac", spanish: true)
        assertWindow(size: CGSize(width: 900, height: 560), spanish: true) { MacServiceDetail(presentation: store) }
    }

    func test_card_noticeWithACommandInItsBody_es() throws {
        let store = try presentation(detail: "card-know-command-es-mac")
        assertWindow(size: CGSize(width: 900, height: 620), spanish: true) { MacServiceDetail(presentation: store) }
    }

    func test_aLinkedDocumentDrawsItsLinksAndImages() async throws {
        let diagram = URL(string: "/files?path=files/images/\(String(repeating: "c", count: 64)).png")!
        let image = NSImage(size: CGSize(width: 320, height: 140), flipped: false) { rect in
            NSColor.systemTeal.setFill()
            rect.fill()
            NSColor.white.setFill()
            CGRect(x: 24, y: 24, width: 110, height: 92).fill()
            CGRect(x: 158, y: 96, width: 130, height: 20).fill()
            CGRect(x: 158, y: 60, width: 96, height: 20).fill()
            return true
        }
        let png = try XCTUnwrap(NSBitmapImageRep(data: try XCTUnwrap(image.tiffRepresentation))?.representation(using: .png, properties: [:]))
        let files = StoredFiles { _ in png }
        _ = try await files.fetch(try XCTUnwrap(StoredImage.attachment(diagram)))
        let readme = """
        # Alondra

        <p align="center"><img src="\(diagram.absoluteString)" alt="How a card reaches you"></p>

        Read <a href="https://example.com/docs">the docs</a> or [the install guide](https://example.com/install).

        - Works with <img src="https://example.com/badge.svg" alt="Homebrew"> on Apple Silicon
        - Press <kbd>v</kbd> to open a document
        """
        let content = MacContent.markdown(MarkdownParser.segments(from: readme))
        assertWindow(size: CGSize(width: 620, height: 520)) {
            MacContentViewer(title: "README.md", content: content, loading: false, error: nil, comments: .constant([:]))
                .environment(\.storedFiles, files)
        }
    }

    /// A private repository's PR as a sandbox daemon served it to the Mac, its description cut to the
    /// screenshots: the images are the copies the daemon brought from GitHub, and one it could not
    /// bring says why.
    func test_card_pullRequestDescriptionShowsTheImagesTheDaemonBrought() async throws {
        try await assertBroughtImages(spanish: false)
    }

    func test_card_pullRequestDescriptionShowsTheImagesTheDaemonBrought_spanish() async throws {
        try await assertBroughtImages(spanish: true)
    }

    private func assertBroughtImages(spanish: Bool, testName: String = #function) async throws {
        let environment = ProcessInfo.processInfo.environment
        let folder = URL(fileURLWithPath: try XCTUnwrap(environment["TEST_SRCDIR"]))
            .appendingPathComponent(environment["TEST_WORKSPACE"] ?? "_main")
            .appendingPathComponent("fixtures/views/pr-images")
        var images: [String: Data] = [:]
        for file in try FileManager.default.contentsOfDirectory(at: folder, includingPropertiesForKeys: nil) where file.pathExtension != "json" {
            images[file.deletingPathExtension().lastPathComponent] = try Data(contentsOf: file)
        }
        let brought = images
        let files = StoredFiles { attachment in try XCTUnwrap(brought[attachment.digest ?? ""]) }
        var state = MacPresentationFeature.State()
        state.sidebarDocument = try document(spanish ? "sidebar-mac-es" : "sidebar-mac")
        let detail = try document(spanish ? "card-es-mac" : "card-mac", folder: "pr-images")
        state.detailDocument = detail
        state.selection = .viewKey(detail.key)
        state.selectedViewKey = detail.key
        let texts = detail.sections.flatMap(\.items).compactMap { item -> String? in
            if case let .markdown(markdown) = item { markdown.text } else { nil }
        }
        let routes = texts.flatMap { $0.matches(of: /\/files\?path=files\/images\/[0-9a-f]{64}\.[a-z]+/).map { String($0.output) } }
        XCTAssertFalse(routes.isEmpty, "the description points at no image the daemon brought")
        for route in routes {
            _ = try await files.fetch(try XCTUnwrap(StoredImage.attachment(try XCTUnwrap(URL(string: route)))))
        }
        let store = Store(initialState: state) { MacPresentationFeature() } withDependencies: {
            $0.viewsClient.fetchDiff = { _ in try await Task.never() }
        }
        assertWindow(size: CGSize(width: 900, height: 2000), spanish: spanish, testName: testName) {
            MacServiceDetail(presentation: store).environment(\.storedFiles, files)
        }
    }

    func test_card_permissionAllowedInTask() throws {
        let store = try presentation(detail: "card-permission-task-mac")
        assertWindow(size: CGSize(width: 900, height: 720)) { MacServiceDetail(presentation: store) }
    }

    func test_card_permissionAllowedInTask_spanish() throws {
        let store = try presentation(detail: "card-permission-task-es-mac", spanish: true)
        assertWindow(size: CGSize(width: 900, height: 720), spanish: true) { MacServiceDetail(presentation: store) }
    }

    func test_task_allowedInTask() throws {
        let store = try presentation(detail: "task-allowed-mac")
        assertWindow(size: CGSize(width: 900, height: 2900)) { MacServiceDetail(presentation: store) }
    }

    func test_task_allowedInTask_spanish() throws {
        let store = try presentation(detail: "task-allowed-es-mac", spanish: true)
        assertWindow(size: CGSize(width: 900, height: 2900), spanish: true) { MacServiceDetail(presentation: store) }
    }

    func test_task_working() throws {
        let store = try presentation(detail: "task-working-mac")
        assertWindow(size: CGSize(width: 900, height: 1500)) { MacServiceDetail(presentation: store) }
    }

    func test_task_working_spanish() throws {
        let store = try presentation(detail: "task-working-es-mac")
        assertWindow(size: CGSize(width: 900, height: 1500), spanish: true) { MacServiceDetail(presentation: store) }
    }

    func test_task_working_foldsOpen() throws {
        let store = try presentation(detail: "task-working-mac") { state in
            state.openSections = ["subagents_finished", "background_finished", "request"]
        }
        assertWindow(size: CGSize(width: 900, height: 1800), variants: [.light]) { MacServiceDetail(presentation: store) }
    }

    func test_pullRequest_detail() throws {
        let store = try presentation(detail: "pull-request-mac")
        assertWindow(size: CGSize(width: 900, height: 760)) { MacServiceDetail(presentation: store) }
    }

    private func diff() throws -> [DiffFile] { UnifiedDiff.parse(try DiffFixture.text()) }

    func test_pullRequest_changesFoldedIntoTheirSummary() throws {
        let files = try diff()
        let store = try presentation(detail: "pull-request-mac") { $0.diff = MacDiff(digest: "abcdef12", files: files, truncated: false) }
        assertWindow(size: CGSize(width: 900, height: 900)) { MacServiceDetail(presentation: store) }
    }

    func test_pullRequest_changesFoldedIntoTheirSummary_spanish() throws {
        let files = try diff()
        let store = try presentation(detail: "pull-request-mac", spanish: true) { $0.diff = MacDiff(digest: "abcdef12", files: files, truncated: false) }
        assertWindow(size: CGSize(width: 900, height: 900), spanish: true) { MacServiceDetail(presentation: store) }
    }

    func test_pullRequest_changesFileByFile() throws {
        let files = try diff()
        let anchor = try XCTUnwrap(files.first?.lines.first { $0.kind == .added }?.anchor)
        let comments = [anchor: ActionComment(anchor: anchor, text: "Keep the long rows behind a toggle", quote: "")]
        assertWindow(size: CGSize(width: 1100, height: 760)) {
            MacChangesView(files: files, truncated: true, highlights: false, comments: .constant(comments))
        }
    }

    func test_pullRequest_changesFileByFile_spanish() throws {
        let files = try diff()
        assertWindow(size: CGSize(width: 1100, height: 760), spanish: true) {
            MacChangesView(files: files, truncated: true, highlights: false, comments: .constant([:]))
        }
    }

    func test_tasks_board() throws {
        let store = try presentation(detail: "tasks-mac")
        assertWindow(size: CGSize(width: 900, height: 1150)) { MacServiceDetail(presentation: store) }
    }

    func test_tasks_board_aDeliveryInTheTurnThatPresentedIt() throws {
        let store = try presentation(detail: "delivered-working-tasks-mac")
        assertWindow(size: CGSize(width: 900, height: 900)) { MacServiceDetail(presentation: store) }
    }

    func test_tasks_board_aDeliveryInTheTurnThatPresentedIt_spanish() throws {
        let store = try presentation(detail: "delivered-working-tasks-mac-es", spanish: true)
        assertWindow(size: CGSize(width: 900, height: 900), spanish: true) { MacServiceDetail(presentation: store) }
    }

    func test_tasks_board_aDeliveryOnceItsTurnEnded() throws {
        let store = try presentation(detail: "delivered-review-tasks-mac")
        assertWindow(size: CGSize(width: 900, height: 900)) { MacServiceDetail(presentation: store) }
    }

    func test_tasks_board_aDeliveryOnceItsTurnEnded_spanish() throws {
        let store = try presentation(detail: "delivered-review-tasks-mac-es", spanish: true)
        assertWindow(size: CGSize(width: 900, height: 900), spanish: true) { MacServiceDetail(presentation: store) }
    }

    func test_tasks_board_finishedOpen() throws {
        let finished = try document("tasks-mac-finished").sections.first { $0.id == "finished" }
        let store = try presentation(detail: "tasks-mac") { state in
            state.openSections = ["finished"]
            state.sectionPages["finished"] = SectionPage(items: finished?.items ?? [], more: finished?.more)
        }
        assertWindow(size: CGSize(width: 900, height: 1500)) { MacServiceDetail(presentation: store) }
    }

    /// A node's last words as its card, written in Markdown: the need keeps its styles on one line
    /// and the reason reads as the blocks it was written in.
    func test_card_markdownInWhatItNeedsAndWhy() throws {
        let card = try markdownCard()
        let store = try presentation(detail: "card-question-mac") { $0.detailDocument = card }
        assertWindow(size: CGSize(width: 900, height: 720)) { MacServiceDetail(presentation: store) }
    }

    private func markdownCard() throws -> ViewDocument {
        let environment = ProcessInfo.processInfo.environment
        let url = URL(fileURLWithPath: try XCTUnwrap(environment["TEST_SRCDIR"]))
            .appendingPathComponent(environment["TEST_WORKSPACE"] ?? "_main")
            .appendingPathComponent("fixtures/views/cards/card-question-mac.json")
        var object = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any])
        var sections = try XCTUnwrap(object["sections"] as? [[String: Any]])
        let index = try XCTUnwrap(sections.firstIndex { $0["id"] as? String == "card" })
        var items = try XCTUnwrap(sections[index]["items"] as? [[String: Any]])
        let need = "Review the **diagram fix** in `MarkdownBlocks.swift`"
        items[0]["need"] = need
        items[0]["title"] = need
        items[0]["why"] = "## Done\n\n- Diagrams draw in **both** apps\n- `make snapshots` passes\n\nOne golden changed: the plan card."
        sections[index]["items"] = items
        object["sections"] = sections
        return try ContractCodec.decode(ViewDocument.self, from: JSONSerialization.data(withJSONObject: object))
    }

    /// A node whose turn ended with nothing for the user: what it wrote in its terminal is a card.
    func test_card_aNodesLastWords_spanish() throws {
        let store = try presentation(detail: "card-unheard-es-mac", spanish: true)
        assertWindow(size: CGSize(width: 900, height: 620), spanish: true) { MacServiceDetail(presentation: store) }
    }

    // MARK: One connection state, every indicator

    /// A view picked in the saved sidebar that never loaded, while the Mac is not answering: the
    /// same problem the window's subtitle names, not «Loading…» forever.
    func test_detail_nothingSavedWhileTheMacIsNotAnswering() throws {
        let store = try presentation { $0.selection = .viewKey("tasks"); $0.selectedViewKey = "tasks" }
        for spanish in [false, true] {
            assertWindow(size: CGSize(width: 900, height: 520), spanish: spanish, variants: spanish ? [.light] : Appearance.allCases) {
                MacServiceDetail(presentation: store).environment(\.connectionState, .unreachable(.macNotAnswering))
            }
        }
    }

    /// A saved view whose reload failed: while the Mac does not answer, the subtitle says so and the
    /// view stays clean, without a second, red «couldn’t load» over it.
    func test_detail_savedWhileTheMacIsNotAnsweringHasNoSecondError() throws {
        let store = try presentation(detail: "tasks-mac") { $0.viewError = "Couldn’t load this from your Mac. Try again." }
        assertWindow(size: CGSize(width: 900, height: 520), variants: [.light]) {
            MacServiceDetail(presentation: store).environment(\.connectionState, .unreachable(.macNotAnswering))
        }
    }

    func test_sidebar_connectingToYourMac() throws {
        let store = Store(initialState: MacPresentationFeature.State()) { MacPresentationFeature() }
        for spanish in [false, true] {
            assertWindow(size: CGSize(width: 320, height: 200), spanish: spanish, variants: spanish ? [.light] : Appearance.allCases) {
                MacServiceSidebar(presentation: store)
            }
        }
    }

    func test_window_nothingSavedWhileAlondraIsNotRunning() throws {
        let store = Store(initialState: MacPresentationFeature.State()) { MacPresentationFeature() }
        for spanish in [false, true] {
            assertWindow(size: CGSize(width: 900, height: 600), spanish: spanish, variants: spanish ? [.light] : Appearance.allCases) {
                MacServiceSidebar(presentation: store)
                    .connectionLostOverlay(.unreachable(.alondraNotRunning), hasSavedData: false, retry: {},
                                           signIn: nil, settings: {}, pairAgain: nil, localNetworkSettings: nil)
            }
        }
    }

    func test_connectionLost_deviceStorage() {
        for spanish in [false, true] {
            assertWindow(size: CGSize(width: 900, height: 600), spanish: spanish, variants: spanish ? [.light] : Appearance.allCases) {
                ConnectionLostView(problem: .deviceStorage, retry: {}, settings: {})
            }
        }
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

    private func newRequest(spanish: Bool = false, edit: (inout NewRequestFeature.State) -> Void) throws -> StoreOf<MacPresentationFeature> {
        var screen = ScreenFeature.State(key: .newRequest())
        screen.document = try document(spanish ? "new-request-mac-es" : "new-request-mac")
        screen.phase = .loaded
        return try presentation { state in
            state.selection = .viewKey("new_request")
            state.selectedViewKey = "new_request"
            state.newRequest.screen = screen
            for field in state.newRequest.form?.fields ?? [] { state.newRequest.values[field.id] = field.value }
            edit(&state.newRequest)
        }
    }

    func test_newRequest_queue() throws {
        let store = try newRequest { state in
            state.values["text"] = .string("Make the queue rows say who asked")
            state.pending.add("r-failed", text: "Rename the Garden project to Demo Garden",
                              phase: .failed("Alondra can’t take requests right now. Try again in a moment."))
            state.pending.add("r-unconfirmed", text: "Add a widget with the queue", phase: .unconfirmed)
            state.pending.add("r-sending", text: "Polish the Alondra beta before Friday: fix the onboarding copy and the empty states.", phase: .sending)
        }
        assertWindow(size: CGSize(width: 900, height: 900)) { MacServiceDetail(presentation: store) }
    }

    func test_newRequest_queue_spanish() throws {
        let store = try newRequest(spanish: true) { state in
            state.pending.add("r-failed", text: "Cambia el nombre del proyecto Garden a Demo Garden",
                              phase: .failed("Alondra no puede aceptar solicitudes ahora. Vuelve a intentarlo en un momento."))
            state.pending.add("r-sending", text: "Pule la beta de Alondra antes del viernes.", phase: .sending)
        }
        assertWindow(size: CGSize(width: 900, height: 900), spanish: true, variants: [.light]) { MacServiceDetail(presentation: store) }
    }

    /// The request this Mac sent, on its way, and the daemon's row for it, named by the operation
    /// that carried it: the queue shows it once.
    func test_newRequest_sentAndShownByTheDaemon_spanish() throws {
        let nonce = "27d86775-cd69-4956-92d0-4a20cd01c813"
        let text = "Bug para el nodo de Redraft. Tenía todas las fotos analizadas, salí de casa y me saltó la alerta de tienes x fotos por analizar."
        var json = try XCTUnwrap(JSONSerialization.jsonObject(
            with: ContractCodec.encode(document("new-request-mac-es"))) as? [String: Any])
        var sections = try XCTUnwrap(json["sections"] as? [[String: Any]])
        let sent = try XCTUnwrap(sections.firstIndex { $0["id"] as? String == "sent" })
        var row = try XCTUnwrap((sections[sent]["items"] as? [[String: Any]])?.first)
        row["id"] = "op_8e8dd8f1687d492b5a16fb35d2ca17cd_1_85_\(nonce)"
        row["title"] = String(text.prefix(100))
        sections[sent]["items"] = [row]
        json["sections"] = sections
        let shown = try ContractCodec.decode(ViewDocument.self, from: JSONSerialization.data(withJSONObject: json))
        let store = try newRequest(spanish: true) { state in
            state.screen.document = shown
            state.pending.add(nonce, text: text, phase: .sending)
        }
        assertWindow(size: CGSize(width: 900, height: 700), spanish: true, variants: [.light]) { MacServiceDetail(presentation: store) }
    }

    func test_newRequest_beforeTheOptionsArrive() throws {
        let store = try presentation { state in
            state.selection = .viewKey("new_request")
            state.selectedViewKey = "new_request"
            state.newRequest.values["text"] = .string("Polish the Alondra beta before Friday.")
            state.newRequest.pending.add("r-waiting", text: "Add a widget with the queue", phase: .waiting(.options))
        }
        assertWindow(size: CGSize(width: 900, height: 700), variants: [.light]) { MacServiceDetail(presentation: store) }
    }

    func test_newRequest_optionsSheet() throws {
        let form = try XCTUnwrap(try document("new-request-mac").sections.flatMap(\.items).compactMap { item -> AlondraContract.Form? in
            if case let .form(form) = item { form } else { nil }
        }.first)
        let fields = form.fields.filter { $0.id != "text" && $0.id != "attachments" }
        let store = Store(initialState: RequestOptionsFeature.State(fields: fields, values: Dictionary(uniqueKeysWithValues: fields.map { ($0.id, $0.value) }))) {
            RequestOptionsFeature()
        }
        assertWindow(size: CGSize(width: 480, height: 420)) { MacRequestOptionsSheet(store: store) }
    }
    func test_settings_codexDeviceCode() throws {
        for spanish in [false, true] {
            var state = ServerSettingsFeature.State(key: .accounts, client: .mac)
            state.document = try document(spanish ? "accounts-device-code-mac-es" : "accounts-device-code-mac")
            let store = Store(initialState: state) { ServerSettingsFeature() }
            assertWindow(size: CGSize(width: 640, height: 700), spanish: spanish, variants: spanish ? [.light] : Appearance.allCases) {
                Form { ServerSettingsSection(store: store) }.formStyle(.grouped)
            }
        }
    }

    func test_settings_codexAccounts() throws {
        for spanish in [false, true] {
            var state = ServerSettingsFeature.State(key: .accounts, client: .mac)
            state.document = try document(spanish ? "accounts-codex-mac-es" : "accounts-codex-mac")
            let store = Store(initialState: state) { ServerSettingsFeature() }
            assertWindow(size: CGSize(width: 640, height: 900), spanish: spanish, variants: spanish ? [.light] : Appearance.allCases) {
                Form { ServerSettingsSection(store: store) }.formStyle(.grouped)
            }
        }
    }

    func test_newRequest_codexModelOptions() throws {
        for spanish in [false, true] {
            let form = try XCTUnwrap(try document(spanish ? "new-request-codex-mac-es" : "new-request-codex-mac").sections.flatMap(\.items).compactMap { item -> AlondraContract.Form? in
                if case let .form(form) = item { form } else { nil }
            }.first)
            let fields = form.fields.filter { $0.id != "text" && $0.id != "attachments" }
            var values = Dictionary(uniqueKeysWithValues: fields.map { ($0.id, $0.value) })
            values["model"] = .string("codex/fixture-general")
            values["reasoning_effort"] = .string("high")
            values["allow_model_substitution"] = .bool(false)
            let store = Store(initialState: RequestOptionsFeature.State(fields: fields, values: values)) { RequestOptionsFeature() }
            assertWindow(size: CGSize(width: 480, height: 560), spanish: spanish, variants: spanish ? [.light] : Appearance.allCases) {
                MacRequestOptionsSheet(store: store)
            }
        }
    }

}
