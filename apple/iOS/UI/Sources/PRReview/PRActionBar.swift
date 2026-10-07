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
                if changes.enabled {
                    Button { store.send(.perform(changes)) } label: {
                        GlassBarLabel(changesTitle(changes))
                    }
                    .buttonStyle(.glass)
                } else {
                    held(changes, systemImage: "text.bubble")
                }
            }
            if let conflicts = review.resolveConflicts {
                if conflicts.enabled {
                    Button { store.send(.perform(conflicts)) } label: {
                        GlassBarLabel(conflicts.label, systemImage: "arrow.triangle.branch")
                    }
                    .buttonStyle(.glassProminent).tint(Palette.dawn)
                } else {
                    held(conflicts, systemImage: "arrow.triangle.branch")
                }
            } else if let merge = review.merge {
                if merge.enabled { mergeMenu(merge) } else { held(merge, systemImage: "square.stack.3d.up") }
            }
        }
    }

    private func changesTitle(_ action: WireAction) -> String {
        store.comments.isEmpty ? action.label : "\(action.label) (\(store.comments.count))"
    }

    /// An action the server holds back: its reason, on the bar's glass, in place of the button;
    /// never a grey button that does nothing. Without a reason there is nothing to show.
    @ViewBuilder private func held(_ action: WireAction, systemImage: String) -> some View {
        if let reason = action.disabledReason {
            GlassBarLabel(reason, systemImage: systemImage)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 16).padding(.vertical, 12)
                .foregroundStyle(.secondary)
                .glassEffect(.regular, in: .capsule)
                .accessibilityElement(children: .combine)
        }
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
