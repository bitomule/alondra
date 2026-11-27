import AlondraContract
import AlondraDesign
import AlondraFeatures
import AppKit
import ComposableArchitecture
import SnapshotTesting
import SwiftUI
import XCTest

/// The specs of a plan in the Mac window: wide, where the steps and the mockup of a scenario sit side
/// by side, and narrow; the plan, a change, a delivery, a decision and every state a screen of specs
/// can be in, each in light and dark and in English and Spanish.
@MainActor
final class MacSpecSheetSnapshotTests: XCTestCase {
    private static let files = StoredFiles { _ in SpecSheetFixtures.mockupPNG }

    override func setUp() async throws {
        _ = NSApplication.shared
        if let url = URL(string: SpecSheetFixtures.imageURL), let attachment = StoredImage.attachment(url) {
            _ = try await Self.files.fetch(attachment)
        }
        _ = await MermaidRenderCache.shared.rendering(.init(source: SpecSheetFixtures.diagram, size: 14))
    }

    private func assertSpecs<Content: View>(
        width: CGFloat = 900, height: CGFloat, file: StaticString = #filePath, testName: String = #function, line: UInt = #line,
        @ViewBuilder _ content: (Bool) -> Content
    ) {
        let size = CGSize(width: width, height: height)
        for spanish in [false, true] {
            for (name, appearance) in [("light", NSAppearance.Name.aqua), ("dark", .darkAqua)] {
                let view = content(spanish)
                    .padding(24)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                    .background(Color(nsColor: .windowBackgroundColor))
                    .environment(\.locale, spanish ? Locale(identifier: "es_ES") : Locale(identifier: "en_US"))
                    .environment(\.storedFiles, Self.files)
                    .environment(\.sheetHandlers, SheetHandlers(onComment: { _ in }, commented: ["billing.partial/together"]))
                let hosting = NSHostingView(rootView: view)
                let window = snapshotWindow(size: size)
                defer { window.close() }
                window.appearance = NSAppearance(named: appearance)
                window.contentView = hosting
                hosting.frame = CGRect(origin: .zero, size: size)
                hosting.layoutSubtreeIfNeeded()
                assertAppSnapshot(of: hosting as NSView, as: .image(perceptualPrecision: 0.98, size: size),
                                  named: spanish ? "\(name)_es" : name, file: file, testName: testName, line: line)
            }
        }
    }

    func test_planInAWideWindow() {
        assertSpecs(height: 1500) { SpecSheetView(sheet: SpecSheetFixtures.plan(spanish: $0)) }
    }

    func test_planInANarrowWindow() {
        assertSpecs(width: 440, height: 2300) { SpecSheetView(sheet: SpecSheetFixtures.plan(spanish: $0)) }
    }

    func test_amendment() {
        assertSpecs(height: 1200) { SpecSheetView(sheet: SpecSheetFixtures.amendment(spanish: $0)) }
    }

    func test_delivery() {
        assertSpecs(height: 1700) { SpecSheetView(sheet: SpecSheetFixtures.delivery(spanish: $0)) }
    }

    func test_deliveryWithEvidenceNotAvailable() {
        assertSpecs(height: 1500) { SpecSheetView(sheet: SpecSheetFixtures.delivery(spanish: $0, evidenceAvailable: false)) }
    }

    func test_validatingNow() {
        assertSpecs(height: 360) { SpecSheetView(sheet: SpecSheetFixtures.validating(spanish: $0)) }
    }

    func test_decision() {
        assertSpecs(height: 520) { DecisionSheetView(sheet: SpecSheetFixtures.decision(spanish: $0)) }
    }

    func test_fortyScenarios() {
        assertSpecs(height: 1300) { SpecSheetView(sheet: SpecSheetFixtures.forty(spanish: $0)) }
    }

    func test_noSpecs() throws {
        let notice = try [SpecSheetFixtures.notice("This plan changes no spec.", tone: .neutral), SpecSheetFixtures.notice("Este plan no cambia ninguna spec.", tone: .neutral)]
        assertSpecs(height: 120) { ViewItemRenderer(item: .notice(notice[$0 ? 1 : 0])) }
    }

    func test_unreadableSpec() throws {
        let notice = try [
            SpecSheetFixtures.notice("Its specs cannot be read: specs/billing/partial.feature:4: unknown tag @idd", tone: .warning),
            SpecSheetFixtures.notice("Sus specs no se pueden leer: specs/billing/partial.feature:4: etiqueta desconocida @idd", tone: .warning),
        ]
        assertSpecs(height: 140) { ViewItemRenderer(item: .notice(notice[$0 ? 1 : 0])) }
    }

    func test_loading() {
        assertSpecs(height: 420) { _ in SheetLoadingView() }
    }

    func test_failedToLoadWithRetry() {
        assertSpecs(height: 220) { SheetFailureView(message: $0 ? "No se pudo cargar desde tu Mac. Inténtalo de nuevo." : "Couldn’t load this from your Mac. Try again.") {} }
    }

    func test_savingAndNotConfirmed() {
        assertSpecs(height: 1500) {
            SpecSheetView(sheet: SpecSheetFixtures.plan(spanish: $0))
                .environment(\.sheetSaving, ["task_specs:t-1:scenario:c1": .saving, "task_specs:t-1:scenario:c2": .overdue])
        }
    }

    func test_pictureViewer() {
        assertSpecs(width: 800, height: 600) { spanish in
            SheetPictureViewer(part: SpecSheetFixtures.plan(spanish: spanish).parts[1], picture: SpecSheetFixtures.picture, files: Self.files)
        }
    }

    func test_stepsEditor() {
        assertSpecs(width: 600, height: 900) { _ in StepsEditorHost(text: SpecSheetFixtures.steps) }
    }

    func test_mermaidEditorWithItsPreview() {
        assertSpecs(width: 600, height: 700) { _ in MermaidEditorHost(code: SpecSheetFixtures.diagram) }
    }

    func test_formWithTheFieldTheMacRefused() throws {
        let form = try ContractCodec.decode(WireForm.self, from: Data(Self.form.utf8))
        var state = ActionFeature.State(dataEpoch: "e", client: .mac, lang: "en")
        state.action = form.submit
        state.fields = form.initialValues
        state.fieldError = ActionFeature.FieldError(field: "steps", message: "Line 2: a step starts with Given, When, Then, And or But")
        let store = Store(initialState: state) { ActionFeature() }
        assertSpecs(width: 600, height: 700) { _ in ActionInputView(store: store, form: form) }
    }

    private static let submit = #"{"id":"spec_edit:t-1@7~sc-c1","label":"Save the scenario","style":"primary","role":"advance","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":true}"#
    private static let form = #"{"id":"task_specs-scenario","fields":[{"id":"title","label":"Title","kind":{"kind":"text"},"value":"Part of an order","required":true,"reload_on_change":false},{"id":"steps","label":"Steps","kind":{"kind":"steps","placeholder":"Given …"},"value":"Given a paid order of 30 EUR\nSomething odd\nThen 20 EUR are left","required":true,"reload_on_change":false}],"submit":"# + submit + #"}"#
}

private struct StepsEditorHost: View {
    @State var text: String
    var body: some View { StepsEditorView(text: $text, placeholder: "Given …") }
}

private struct MermaidEditorHost: View {
    @State var code: String
    var body: some View { MermaidEditorView(code: $code) }
}
