import AlondraAppComposition
import AlondraSettingsFeature
import ComposableArchitecture
import Observation
import SwiftUI

struct ServiceSettingsScene: View {
    let services: CloudAppServices
    let owner: CloudPairingOwner
    let notifications: ServiceNotificationRuntime
    let context: ServiceSettingsContext
    var body: some View {
        Group {
            if let store = context.store { ServiceSettingsView(store: store, ink: Palette.ink, success: Palette.sage, failure: Palette.rose) }
            else { ProgressView("Loading settings…") }
        }.task { await context.connect(services: services, owner: owner, notifications: notifications) }
    }
}

@MainActor @Observable final class ServiceSettingsContext {
    private(set) var store: StoreOf<ServiceSettingsFeature>?
    func connect(services: CloudAppServices, owner: CloudPairingOwner, notifications: ServiceNotificationRuntime) async {
        guard store == nil else { return }
        let api = await services.api
        let client = notifications.settingsClient(services: services, owner: owner)
        guard !Task.isCancelled else { return }
        store = Store(initialState: ServiceSettingsFeature.State(service: api.base.host() ?? api.base.absoluteString,
            device: api.device, supportsNotifications: true, automaticallySavesQuietHours: true)) { ServiceSettingsFeature() } withDependencies: {
                $0.serviceSettings = client
            }
    }
}
