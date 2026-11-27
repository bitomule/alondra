import AlondraContract
import AlondraDesign
import SwiftUI

/// A spec drawn with the edits the Mac has not confirmed yet applied to it.
struct EditedSpecSheet: View {
    let sheet: SpecSheet
    let onLink: (AlondraContract.Link) -> Void
    let onAction: (AlondraContract.Action) -> Void
    @Environment(\.specEdits) private var edits

    var body: some View {
        if let shown = edits.patched(sheet) {
            SpecSheetView(sheet: shown)
                .environment(\.sheetSaving, edits.saving)
                .transformEnvironment(\.sheetHandlers) { $0.onLink = onLink; $0.onAction = onAction }
        }
    }
}

struct EditedDecisionSheet: View {
    let sheet: DecisionSheet
    let onLink: (AlondraContract.Link) -> Void
    let onAction: (AlondraContract.Action) -> Void
    @Environment(\.specEdits) private var edits

    var body: some View {
        if let shown = edits.patched(sheet) {
            DecisionSheetView(sheet: shown)
                .environment(\.sheetSaving, edits.saving)
                .transformEnvironment(\.sheetHandlers) { $0.onLink = onLink; $0.onAction = onAction }
        }
    }
}
