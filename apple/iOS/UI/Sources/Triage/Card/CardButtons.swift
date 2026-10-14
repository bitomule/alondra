import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// The buttons the server gives the card, in its order, the last beside «More» when there is more.
/// Buttons with options, and More, ask in a dialog anchored to them, never a menu: iOS 27 draws a
/// closing menu's button above everything else while it folds back, over the stamp.
struct CardButtons: View {
    let store: StoreOf<TriageCardFeature>
    let cardID: String
    @State private var choosing: AlondraContract.Action?
    @State private var showsMore = false
    @Environment(\.openURL) private var openURL

    var body: some View {
        GlassEffectContainer(spacing: 8) {
            VStack(spacing: 8) {
                ForEach(Array(store.buttons.dropLast().enumerated()), id: \.offset) { _, action in
                    button(action)
                }
                HStack(alignment: .top, spacing: 8) {
                    if let last = store.buttons.last { button(last) }
                    if !store.menu.isEmpty { more }
                }
            }
            .controlSize(.large)
            .buttonBorderShape(.capsule)
            .font(.headline)
        }
        .opacity(store.stamp == nil ? 1 : 0)
    }

    private func button(_ action: AlondraContract.Action) -> some View {
        VStack(spacing: 4) {
            if action.presentation == .menu, !action.options.isEmpty {
                Button { choosing = action } label: { GlassBarLabel(action.label) }
                    .glassRole(action.cardRole)
                    .accessibilityIdentifier(identifier(action))
                    .confirmationDialog(
                        action.label,
                        isPresented: Binding(get: { choosing == action }, set: { if !$0 { choosing = nil } }),
                        titleVisibility: .hidden
                    ) {
                        ForEach(action.options, id: \.id) { option in
                            Button(option.label) { store.send(.menuOptionPicked(action, optionID: option.id)) }
                        }
                    }
            } else {
                Button { store.perform(action, openURL: openURL) } label: { GlassBarLabel(action.label).multilineTextAlignment(.center) }
                    .glassRole(action.cardRole)
                    .accessibilityIdentifier(identifier(action))
            }
            ButtonCaption(action: action)
        }
    }

    private func identifier(_ action: AlondraContract.Action) -> String {
        "triage.action.\(cardID).\(action.optionId ?? action.label)"
    }

    private var more: some View {
        Button { showsMore = true } label: { Image(systemName: "ellipsis.circle") }
            .buttonStyle(.glass)
            .accessibilityLabel("More actions")
            .confirmationDialog("More actions", isPresented: $showsMore, titleVisibility: .hidden) {
                ForEach(Array(store.menu.enumerated()), id: \.offset) { _, action in
                    Button(action.label) { store.perform(action, openURL: openURL) }
                }
            }
    }
}
