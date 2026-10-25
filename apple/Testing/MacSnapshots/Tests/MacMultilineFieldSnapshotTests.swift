import AlondraContract
import AlondraDesign
import AlondraFeatures
import AppKit
import ComposableArchitecture
import SnapshotTesting
import SwiftUI
import XCTest

/// The Mac's multi-line fields with a minimum height: an action's written answer, a form's
/// multiline and lines fields, the diff line comment and the pairing invitation's field. Each keeps
/// its minimum lines inside a bezel, whatever line height SwiftUI cached for another font.
@MainActor
final class MacMultilineFieldSnapshotTests: XCTestCase {
    override func setUp() async throws {
        _ = NSApplication.shared
    }

    private static let reply = #"{"id":"reply","label":"Reply","style":"secondary","role":"neutral","presentation":"button","input":{"kind":"text","placeholder":"Reply to Alondra…","required":true,"multiline":true},"attachments":false,"enabled":true}"#
    private static let send = #"{"id":"send","label":"Send","style":"primary","role":"neutral","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":true}"#
    private static let form = #"{"id":"brief","fields":[{"id":"mission","label":"Mission","kind":{"kind":"multiline","placeholder":"What should the node do?"},"value":"","required":true,"reload_on_change":false},{"id":"paths","label":"Paths","kind":{"kind":"lines","placeholder":"One path per line"},"value":"","required":false,"reload_on_change":false}],"submit":"# + send + #"}"#

    private func actionStore(_ json: String, text: String = "") throws -> StoreOf<ActionFeature> {
        var state = ActionFeature.State(dataEpoch: "e", client: .mac, lang: "en")
        state.action = try ContractCodec.decode(WireAction.self, from: Data(json.utf8))
        state.text = text
        return Store(initialState: state) { ActionFeature() }
    }

    private func assertField<Content: View>(
        height: CGFloat, file: StaticString = #filePath, testName: String = #function, line: UInt = #line,
        @ViewBuilder _ content: () -> Content
    ) {
        let size = CGSize(width: 520, height: height)
        let view = content()
            .padding(20)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .background(Color(nsColor: .windowBackgroundColor))
        for (name, appearance) in [("light", NSAppearance.Name.aqua), ("dark", .darkAqua)] {
            let hosting = NSHostingView(rootView: view)
            let window = snapshotWindow(size: size)
            defer { window.close() }
            window.appearance = NSAppearance(named: appearance)
            window.contentView = hosting
            hosting.frame = CGRect(origin: .zero, size: size)
            hosting.layoutSubtreeIfNeeded()
            assertAppSnapshot(of: hosting as NSView, as: .image(perceptualPrecision: 0.98, size: size),
                              named: name, file: file, testName: testName, line: line)
        }
    }

    func test_actionAnswer_emptyShowsFourLines() throws {
        let store = try actionStore(Self.reply)
        assertField(height: 220) { ActionInputView(store: store) }
    }

    func test_actionAnswer_written() throws {
        let store = try actionStore(Self.reply, text: "Ship the concise copy.\nKeep the warmer one for the website.")
        assertField(height: 220) { ActionInputView(store: store) }
    }

    func test_formFields_multilineAndLines() throws {
        let store = try actionStore(Self.send)
        let form = try ContractCodec.decode(WireForm.self, from: Data(Self.form.utf8))
        assertField(height: 340) { ActionInputView(store: store, form: form) }
    }

    func test_lineComment() throws {
        let diff = "diff --git a/App.swift b/App.swift\n--- a/App.swift\n+++ b/App.swift\n@@ -1,1 +1,1 @@\n-let title = \"Hi\"\n+let title = \"Hello\"\n"
        let line = try XCTUnwrap(UnifiedDiff.parse(diff).flatMap(\.lines).first { $0.kind == .added })
        assertField(height: 300) { LineCommentSheet(line: line, initial: "", text: .constant(""), onSave: { _ in }) }
    }

    func test_invitationField_emptyAndWritten() {
        assertField(height: 240) {
            VStack(alignment: .leading, spacing: 16) {
                MultilineTextField("Connection invitation", text: .constant(""), lines: 3...6)
                MultilineTextField("Connection invitation", text: .constant("alondra://pair?v=1&k=Q2FyZ2FuZG8gbGEgaW52aXRhY2nDs24gZGUgcHJ1ZWJh"), lines: 3...6)
            }
        }
    }
}
