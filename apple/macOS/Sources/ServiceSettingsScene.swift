import AlondraAppComposition
import AlondraCloudPairingFeature
import AlondraSettingsFeature
import ComposableArchitecture
import Observation
import SwiftUI

struct MacServiceSettingsScene: View {
    let services: CloudAppServices
    let owner: CloudPairingOwner
    @State private var context = MacSettingsContext()
    var body: some View {
        Group {
            if let store = context.store { ServiceSettingsView(store: store) }
            else { ProgressView("Loading settings…") }
        }.task { await context.connect(services: services, owner: owner) }
    }
}

@MainActor @Observable private final class MacSettingsContext {
    private(set) var store: StoreOf<ServiceSettingsFeature>?
    func connect(services: CloudAppServices, owner: CloudPairingOwner) async {
        guard store == nil else { return }
        let api = await services.api
        let client = ServiceSettingsClient(observe: { AsyncStream { $0.finish() } },
            save: { _, _ in throw CancellationError() }, requestPermission: {}, openSystemSettings: {},
            checkConnection: { try await services.checkSettingsConnection() },
            logout: { await MainActor.run {
                guard owner.services === services else { return }
                _ = owner.store.send(.logout)
            } })
        guard !Task.isCancelled else { return }
        store = Store(initialState: ServiceSettingsFeature.State(service: api.base.host() ?? api.base.absoluteString,
            device: api.device, supportsNotifications: false)) { ServiceSettingsFeature() } withDependencies: {
                $0.serviceSettings = client
            }
    }
}
