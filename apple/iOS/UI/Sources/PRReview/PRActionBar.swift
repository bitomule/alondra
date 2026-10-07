import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// The PR's buttons: request changes, then the main action (merge by method, or hand the
/// conflicts back). Opening the PR on its host lives in the toolbar.
struct PRActionBar: View {
    let store: StoreOf<PRReviewFeature>
    let review: PRReview

    var body: some View {
        GlassActionBar {
            if let changes = review.requestChanges {
                Button { store.send(.perform(changes)) } label: {
                    GlassBarLabel(changesTitle(changes))
                }
                .buttonStyle(.glass)
                .disabled(!changes.enabled)
            }
            if let conflicts = review.resolveConflicts {
                Button { store.send(.perform(conflicts)) } label: {
                    GlassBarLabel(conflicts.label, systemImage: "arrow.triangle.branch")
                }
                .buttonStyle(.glassProminent).tint(Palette.dawn)
                .disabled(!conflicts.enabled)
            } else if let merge = review.merge {
                if merge.enabled { mergeMenu(merge) } else { blocked(merge) }
            }
        }
    }

    private func changesTitle(_ action: WireAction) -> String {
        store.comments.isEmpty ? action.label : "\(action.label) (\(store.comments.count))"
    }

    /// A merge the server holds back: its reason in place of the button, never a button that does nothing.
    private func blocked(_ merge: WireAction) -> some View {
        GlassBarLabel(merge.disabledReason ?? merge.label, systemImage: "square.stack.3d.up")
            .padding(.horizontal, 12).padding(.vertical, 10)
            .foregroundStyle(.secondary)
            .accessibilityElement(children: .combine)
    }

    private func mergeMenu(_ merge: WireAction) -> some View {
        Menu {
            ForEach(merge.options, id: \.id) { option in
                Button(option.label) { store.send(.perform(merge, optionID: option.id)) }
            }
        } label: {
            GlassBarLabel("\(merge.label)…", systemImage: "arrow.triangle.merge")
        }
        .menuStyle(.button).buttonStyle(.glassProminent)
        .tint(Palette.sageDeep)
    }
}
