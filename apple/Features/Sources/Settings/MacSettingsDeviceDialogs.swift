import ComposableArchitecture
import SwiftUI

/// The confirmations before forgetting another Mac or revoking a device of this one.
struct MacSettingsDeviceDialogs: ViewModifier {
    @Bindable var store: StoreOf<SettingsFeature>

    func body(content: Content) -> some View {
        content
            .confirmationDialog("Forget this Mac?", isPresented: Binding(get: { store.confirmForgetMacID != nil },
                                                                       set: { if !$0 { store.send(.cancelForgetMac) } }),
                                titleVisibility: .visible) {
                Button("Forget Mac", role: .destructive) { store.send(.confirmForgetMac) }
                Button("Cancel", role: .cancel) { store.send(.cancelForgetMac) }
            } message: {
                Text("This Mac deletes its keys for the other Mac. To use it again, find it and pair it again.")
            }
            .confirmationDialog("Revoke this device?", isPresented: Binding(get: { store.confirmRevokeDeviceID != nil },
                                                                          set: { if !$0 { store.send(.cancelRevokeDevice) } }),
                                titleVisibility: .visible) {
                Button("Revoke", role: .destructive) { store.send(.confirmRevokeDevice) }
                Button("Cancel", role: .cancel) { store.send(.cancelRevokeDevice) }
            } message: {
                Text("It loses access to this Mac at once, on the local network and through the relay.")
            }
            .task { await store.send(.loadDevices).finish() }
    }
}
