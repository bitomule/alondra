import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// The PR's buttons: ask or request changes, the main action (merge by method, or hand the
/// conflicts back), and a quiet link to the PR on its host.
struct PRActionBar: View {
    let store: StoreOf<PRReviewFeature>
    let review: PRReview
    let openURL: OpenURLAction

    var body: some View {
        let busy = store.actions.inFlight
        VStack(spacing: 8) {
            if review.ask != nil || review.requestChanges != nil {
                HStack(spacing: 8) {
                    if let ask = review.ask {
                        Button { store.send(.perform(ask)) } label: {
                            Label(ask.label, systemImage: "questionmark.bubble").frame(maxWidth: .infinity)
                        }
                        .buttonStyle(.glass)
                        .disabled(!ask.enabled || busy)
                    }
                    if let changes = review.requestChanges {
                        Button { store.send(.perform(changes)) } label: {
                            Label(changesTitle(changes), systemImage: "arrow.uturn.backward").frame(maxWidth: .infinity)
                        }
                        .buttonStyle(.glassProminent)
                        .disabled(!changes.enabled || busy)
                    }
                }
            }
            if let conflicts = review.resolveConflicts {
                Button { store.send(.perform(conflicts)) } label: {
                    Label(conflicts.label, systemImage: "arrow.triangle.branch").frame(maxWidth: .infinity)
                }
                .buttonStyle(.glassProminent).tint(Palette.dawn)
                .disabled(!conflicts.enabled || busy)
            } else if let merge = review.merge {
                mergeMenu(merge, busy: busy)
            }
            if let url = review.pageURL {
                HStack {
                    Button { openURL(url) } label: {
                        Label("Open on GitHub", systemImage: "arrow.up.right.square")
                    }
                    Spacer()
                }
                .font(.subheadline.weight(.medium)).foregroundStyle(.secondary)
                .padding(.horizontal, 4)
            }
        }
        .controlSize(.large)
        .buttonBorderShape(.capsule)
        .font(.headline)
        .padding(.horizontal, 16).padding(.vertical, 10)
        .background(.bar)
    }

    private func changesTitle(_ action: WireAction) -> String {
        store.comments.isEmpty ? action.label : "\(action.label) (\(store.comments.count))"
    }

    private func mergeMenu(_ merge: WireAction, busy: Bool) -> some View {
        Menu {
            ForEach(merge.options, id: \.id) { option in
                Button(option.label) { store.send(.perform(merge, optionID: option.id)) }
            }
        } label: {
            Label(merge.enabled ? "\(merge.label)…" : (merge.disabledReason ?? merge.label),
                  systemImage: merge.enabled ? "arrow.triangle.merge" : "square.stack.3d.up")
                .frame(maxWidth: .infinity)
        }
        .menuStyle(.button).buttonStyle(.glassProminent)
        .tint(merge.enabled ? Palette.sageDeep : Palette.slate)
        .disabled(!merge.enabled || busy)
    }
}
