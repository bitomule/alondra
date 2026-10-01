import AlondraFeatures
import ComposableArchitecture
import SwiftUI

extension View {
    /// The questions an action can raise before it runs: the daemon's confirmation, and the
    /// notice that what the action meant has changed since the screen was drawn.
    func actionDialogs(_ store: StoreOf<ActionFeature>) -> some View {
        modifier(ActionDialogs(store: store))
    }
}

private struct ActionDialogs: ViewModifier {
    @Bindable var store: StoreOf<ActionFeature>

    func body(content: Content) -> some View {
        content
            .confirmationDialog(
                store.confirmation?.title ?? "",
                isPresented: Binding(
                    get: { store.confirmation != nil },
                    set: { if !$0 { store.send(.cancelConfirmation) } }
                ),
                titleVisibility: .visible
            ) {
                Button(store.confirmation?.button ?? "", role: .destructive) { store.send(.confirm) }
            } message: {
                Text(verbatim: store.confirmation?.message ?? "")
            }
            .alert(
                "This changed",
                isPresented: Binding(
                    get: { store.changed != nil },
                    set: { if !$0 { store.send(.dismiss) } }
                )
            ) {
                Button(store.changed?.button ?? "") { store.send(.sendChangedAction) }
                Button("Cancel", role: .cancel) { store.send(.dismiss) }
            } message: {
                Text(verbatim: store.error ?? "")
            }
    }
}
