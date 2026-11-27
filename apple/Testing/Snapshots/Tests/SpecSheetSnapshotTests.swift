import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SnapshotTesting
import SwiftUI
import UIKit
import XCTest

/// The specs of a plan on the phone: the plan, a change to approved specs, a delivery, a decision and
/// every state a screen of specs can be in, each in light and dark and in English and Spanish, and at the
/// largest accessibility text for the ones a person reads closely.
@MainActor
final class SpecSheetSnapshotTests: XCTestCase {
    private static let files = StoredFiles { _ in SpecSheetFixtures.mockupPNG }

    override func setUp() async throws {
        if let url = URL(string: SpecSheetFixtures.imageURL), let attachment = StoredImage.attachment(url) {
            _ = try await Self.files.fetch(attachment)
        }
        let sizes = [UIContentSizeCategory.large, .accessibilityExtraLarge].map { category in
            UIFontMetrics(forTextStyle: .subheadline)
                .scaledValue(for: 14, compatibleWith: UITraitCollection(preferredContentSizeCategory: category)).rounded()
        }
        for size in sizes { _ = await MermaidRenderCache.shared.rendering(.init(source: SpecSheetFixtures.diagram, size: size)) }
    }

    /// Light and dark in both languages; `large` adds the accessibility size.
    private func assertSpecs<Content: View>(
        large: Bool = false, file: StaticString = #filePath, testName: String = #function, line: UInt = #line,
        @ViewBuilder _ content: (Bool) -> Content
    ) {
        for language in [SnapshotLanguage.english, .spanish] {
            let spanish = language == .spanish
            for variant in large ? SnapshotVariant.allCases : [.light, .dark] {
                let name = spanish ? "\(variant.suffix)_es" : variant.suffix
                let view = screen(variant, language: language) {
                    content(spanish)
                        .padding(16)
                        .frame(maxWidth: .infinity, alignment: .topLeading)
                        .background(Palette.night)
                        .environment(\.storedFiles, Self.files)
                        .environment(\.sheetHandlers, SheetHandlers(onComment: { _ in }, commented: ["billing.partial/together"]))
                }
                assertAppSnapshot(of: view, as: deviceImage(variant), named: name, file: file, testName: testName, line: line)
            }
        }
    }

    func test_plan() {
        assertSpecs(large: true) { SpecSheetView(sheet: SpecSheetFixtures.plan(spanish: $0)) }
    }

    func test_amendment() {
        assertSpecs { SpecSheetView(sheet: SpecSheetFixtures.amendment(spanish: $0)) }
    }

    func test_delivery() {
        assertSpecs(large: true) { SpecSheetView(sheet: SpecSheetFixtures.delivery(spanish: $0)) }
    }

    func test_deliveryWithEvidenceNotAvailable() {
        assertSpecs { SpecSheetView(sheet: SpecSheetFixtures.delivery(spanish: $0, evidenceAvailable: false)) }
    }

    func test_validatingNow() {
        assertSpecs { SpecSheetView(sheet: SpecSheetFixtures.validating(spanish: $0)) }
    }

    func test_decision() {
        assertSpecs { DecisionSheetView(sheet: SpecSheetFixtures.decision(spanish: $0)) }
    }

    func test_fortyScenarios() {
        assertSpecs { SpecSheetView(sheet: SpecSheetFixtures.forty(spanish: $0)) }
    }

    func test_noSpecs() throws {
        let notice = try [SpecSheetFixtures.notice("This plan changes no spec.", tone: .neutral), SpecSheetFixtures.notice("Este plan no cambia ninguna spec.", tone: .neutral)]
        assertSpecs { ViewItemRenderer(item: .notice(notice[$0 ? 1 : 0])) }
    }

    func test_unreadableSpec() throws {
        let notice = try [
            SpecSheetFixtures.notice("Its specs cannot be read: specs/billing/partial.feature:4: unknown tag @idd", tone: .warning),
            SpecSheetFixtures.notice("Sus specs no se pueden leer: specs/billing/partial.feature:4: etiqueta desconocida @idd", tone: .warning),
        ]
        assertSpecs { ViewItemRenderer(item: .notice(notice[$0 ? 1 : 0])) }
    }

    func test_loading() {
        assertSpecs { _ in SheetLoadingView() }
    }

    func test_failedToLoadWithRetry() {
        assertSpecs { SheetFailureView(message: $0 ? "No se pudo cargar desde tu Mac. Inténtalo de nuevo." : "Couldn’t load this from your Mac. Try again.") {} }
    }

    func test_savingAndNotConfirmed() {
        assertSpecs {
            SpecSheetView(sheet: SpecSheetFixtures.plan(spanish: $0))
                .environment(\.sheetSaving, ["task_specs:t-1:scenario:c1": .saving, "task_specs:t-1:scenario:c2": .overdue])
        }
    }

    func test_pictureViewer() {
        assertSpecs { spanish in
            SheetPictureViewer(part: SpecSheetFixtures.plan(spanish: spanish).parts[1], picture: SpecSheetFixtures.picture, files: Self.files)
                .frame(height: 640)
        }
    }

    func test_stepsEditor() {
        assertSpecs(large: true) { _ in StepsEditorHost(text: SpecSheetFixtures.steps) }
    }

    func test_stepsThatAreNotStepsStayText() {
        assertSpecs { _ in StepsEditorHost(text: "Some words that are not a step yet") }
    }

    func test_mermaidEditorWithItsPreview() {
        assertSpecs { _ in MermaidEditorHost(code: SpecSheetFixtures.diagram) }
    }

    func test_formWithTheFieldTheMacRefused() throws {
        let form = try ContractCodec.decode(WireForm.self, from: Data(Self.form.utf8))
        var state = ActionFeature.State(dataEpoch: "e", client: .ios, lang: "en")
        state.action = form.submit
        state.fields = form.initialValues
        state.fieldError = ActionFeature.FieldError(field: "steps", message: "Line 2: a step starts with Given, When, Then, And or But")
        let store = Store(initialState: state) { ActionFeature() }
        assertSpecs { _ in ActionInputView(store: store, form: form) }
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
