import ComposableArchitecture
import SwiftUI

public extension View {
    /// The two questions an action can raise, asked the same way on iPhone and Mac: the server's
    /// confirmation, and the notice that what the person looked at has changed. The words come from
    /// the server; the confirming button is red only when the action itself is destructive.
    func actionDialogs(_ store: StoreOf<ActionFeature>) -> some View {
        modifier(ActionDialogs(store: store))
    }
}

private struct ActionDialogs: ViewModifier {
    @Bindable var store: StoreOf<ActionFeature>

    func body(content: Content) -> some View {
        content
            .alert(
                store.confirmation?.title ?? "",
                isPresented: Binding(
                    get: { store.confirmation != nil },
                    set: { if !$0 { store.send(.cancelConfirmation) } }
                )
            ) {
                Button(store.confirmation?.button ?? "", role: store.confirmationIsDestructive ? .destructive : nil) {
                    store.send(.confirm)
                }
                Button("Cancel", role: .cancel) { store.send(.cancelConfirmation) }
            } message: {
                Text(verbatim: store.confirmation?.message ?? "")
            }
            .alert(
                store.changed?.title ?? "",
                isPresented: Binding(
                    get: { store.changed != nil },
                    set: { if !$0 { store.send(.dismissChanged) } }
                )
            ) {
                Button(store.changed?.button ?? "") { store.send(.sendChangedAction) }
                Button("Cancel", role: .cancel) { store.send(.dismissChanged) }
            } message: {
                Text(verbatim: store.changed?.body ?? "")
            }
    }
}
