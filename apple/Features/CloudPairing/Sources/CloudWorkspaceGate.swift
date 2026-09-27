import ComposableArchitecture
import SwiftUI

/// The production scene's routing decision, shared with isolated visual hosts.
/// Credentials and service construction remain outside this presentation boundary.
public struct CloudWorkspaceGate<Approved: View, Restoring: View, Onboarding: View>: View {
    let store: StoreOf<CloudPairingFeature>
    let hasServices: Bool
    let approved: () -> Approved
    let restoring: () -> Restoring
    let onboarding: () -> Onboarding

    public init(store: StoreOf<CloudPairingFeature>, hasServices: Bool,
                @ViewBuilder approved: @escaping () -> Approved,
                @ViewBuilder restoring: @escaping () -> Restoring,
                @ViewBuilder onboarding: @escaping () -> Onboarding) {
        self.store = store; self.hasServices = hasServices
        self.approved = approved; self.restoring = restoring; self.onboarding = onboarding
    }

    public var body: some View {
        if hasServices { approved() }
        else if store.showsWorkspace { restoring() }
        else { onboarding() }
    }
}
